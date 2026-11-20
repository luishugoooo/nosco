use std::cmp::Ordering;

use unicode_normalization::UnicodeNormalization;

use crate::{
    BoundingBox, GraphicKind, GraphicObject, LayoutBlock, LayoutDocument, LayoutLine, LayoutToken,
    PageInfo,
    fonts::Font,
    interpreter::{Direction, Glyph},
};

struct Row {
    baseline: f64,
    size: f64,
    glyphs: Vec<Glyph>,
}

struct Line {
    bbox: BoundingBox,
    baseline: f64,
    size: f64,
    start: f64,
    end: f64,
    style: (bool, bool),
    glyphs: Vec<Glyph>,
}

impl Line {
    fn new(glyphs: Vec<Glyph>, baseline: f64, size: f64) -> Self {
        let mut line = Self {
            bbox: glyphs[0].bbox,
            baseline,
            size,
            start: glyphs[0].start,
            end: glyphs[0].end,
            style: (false, false),
            glyphs,
        };
        line.update_bounds();
        line
    }

    fn update_bounds(&mut self) {
        for glyph in &self.glyphs {
            self.bbox = self.bbox.union(&glyph.bbox);
            self.start = self.start.min(glyph.start);
            self.end = self.end.max(glyph.end);
        }
    }

    fn sort_glyphs(&mut self) {
        self.glyphs.sort_by(|a, b| a.start.total_cmp(&b.start));
        self.glyphs.dedup_by(|a, b| {
            a.text == b.text
                && (a.start - b.start).abs() < 0.22
                && (a.baseline - b.baseline).abs() < 0.22
                && (a.size - b.size).abs() < 0.2
        });
    }

    fn update_style(&mut self, fonts: &[Font]) {
        let mut sizes: Vec<_> = self
            .glyphs
            .iter()
            .filter(|glyph| visible(glyph))
            .map(|glyph| glyph.size)
            .collect();
        sizes.sort_unstable_by(f64::total_cmp);
        if let Some(&size) = sizes.get(sizes.len() / 2) {
            self.size = size;
        }
        let mut styles = [0; 4];
        for glyph in self
            .glyphs
            .iter()
            .filter(|glyph| visible(glyph) && (glyph.size - self.size).abs() < 0.5)
        {
            let font = &fonts[glyph.font];
            styles[2 * usize::from(font.bold) + usize::from(font.italic)] += 1;
        }
        let index = (0..4)
            .max_by_key(|&index| (styles[index], 4 - index))
            .unwrap_or(0);
        self.style = (index & 2 != 0, index & 1 != 0);
    }
}

fn visible(glyph: &Glyph) -> bool {
    !glyph.text.as_str().chars().all(char::is_whitespace)
}

pub(crate) fn append_page(
    doc: &mut LayoutDocument,
    info: &mut PageInfo,
    glyphs: &mut Vec<Glyph>,
    fonts: &[Font],
    graphics: &[GraphicObject],
) {
    glyphs.sort_by(|a, b| {
        a.direction
            .cmp(&b.direction)
            .then_with(|| (a.baseline + a.rise).total_cmp(&(b.baseline + b.rise)))
            .then_with(|| a.start.total_cmp(&b.start))
    });
    let mut horizontal = Vec::new();
    let mut rotated = Vec::new();
    let all = glyphs.drain(..);
    let mut groups = all.peekable();
    while let Some(first) = groups.next() {
        let direction = first.direction;
        let group = std::iter::once(first).chain(std::iter::from_fn(|| {
            if groups
                .peek()
                .is_some_and(|glyph| glyph.direction == direction)
            {
                groups.next()
            } else {
                None
            }
        }));
        let lines = cluster_lines(group, fonts);
        if direction == Direction::Right {
            horizontal = lines;
        } else {
            rotated.push(lines);
        }
    }

    if let Some(area) = main_area(&horizontal, info.height).or_else(|| {
        rotated
            .iter()
            .flatten()
            .map(|line| line.bbox)
            .reduce(|a, b| a.union(&b))
    }) {
        info.main_area = area;
    }
    let mut regions = Vec::new();
    xy_cut(horizontal, &mut regions, graphics, 0);
    regions.extend(rotated);
    for region in regions {
        append_region(doc, info.number, region, fonts);
    }

    info.block_range.end = doc.blocks.len();
    info.line_range.end = doc.lines.len();
    info.token_range.end = doc.tokens.len();
}

fn cluster_lines(glyphs: impl IntoIterator<Item = Glyph>, fonts: &[Font]) -> Vec<Line> {
    let mut rows: Vec<Row> = Vec::new();
    for glyph in glyphs {
        let anchor = glyph.baseline + glyph.rise;
        let row = rows
            .iter_mut()
            .rev()
            .take_while(|row| anchor - row.baseline <= 12.0)
            .filter_map(|row| {
                let delta = (anchor - row.baseline).abs();
                (delta <= (0.23 * glyph.size.min(row.size)).clamp(0.7, 7.0)).then_some((row, delta))
            })
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(row, _)| row);
        if let Some(row) = row {
            if glyph.size > row.size {
                row.size = glyph.size;
            }
            row.glyphs.push(glyph);
        } else {
            rows.push(Row {
                baseline: anchor,
                size: glyph.size,
                glyphs: vec![glyph],
            });
        }
    }

    let mut fragments = Vec::new();
    for Row {
        baseline,
        size,
        mut glyphs,
    } in rows
    {
        glyphs.sort_by(|a, b| a.start.total_cmp(&b.start));
        let mut fragment = Vec::new();
        let mut last_end = f64::NEG_INFINITY;
        let mut last_size: f64 = size;
        for glyph in glyphs {
            let threshold = (0.95 * glyph.size.min(last_size)).clamp(6.5, 16.0);
            if glyph.start - last_end > threshold && fragment.iter().any(visible) {
                fragments.push(Line::new(trim_spaces(fragment), baseline, size));
                fragment = Vec::new();
                last_end = f64::NEG_INFINITY;
            }
            last_end = last_end.max(glyph.end);
            last_size = glyph.size;
            fragment.push(glyph);
        }
        if fragment.iter().any(visible) {
            fragments.push(Line::new(trim_spaces(fragment), baseline, size));
        }
    }

    // A small run placed with its own text matrix often sits outside the
    // baseline tolerance. Attach it only to an adjacent, larger run.
    for line in &mut fragments {
        line.update_style(fonts);
    }
    attach_scripts(&mut fragments);
    for line in &mut fragments {
        line.sort_glyphs();
        compose_accents(&mut line.glyphs);
        line.update_style(fonts);
    }
    fragments
}

fn trim_spaces(mut glyphs: Vec<Glyph>) -> Vec<Glyph> {
    let start = glyphs.iter().position(visible).unwrap_or(0);
    let end = glyphs
        .iter()
        .rposition(visible)
        .map_or(start, |idx| idx + 1);
    glyphs.truncate(end);
    glyphs.drain(..start);
    glyphs
}

fn attach_scripts(lines: &mut Vec<Line>) {
    lines.sort_by(|a, b| a.baseline.total_cmp(&b.baseline));
    let mut targets = vec![None; lines.len()];
    for (index, script) in lines.iter().enumerate() {
        if script.glyphs.len() > 14 {
            continue;
        }
        let is_accent =
            script.glyphs.len() == 1 && accent(script.glyphs[0].text.as_str()).is_some();
        let first = lines.partition_point(|line| line.baseline < script.baseline - 15.0);
        let mut best: Option<(usize, f64)> = None;
        for (target, line) in lines
            .iter()
            .enumerate()
            .skip(first)
            .take_while(|(_, line)| line.baseline <= script.baseline + 15.0)
        {
            let delta = (script.baseline - line.baseline).abs();
            if is_accent {
                if target != index
                    && delta <= 0.5 * line.size
                    && line
                        .glyphs
                        .iter()
                        .any(|letter| accent_distance(&script.glyphs[0], letter).is_some())
                    && best.is_none_or(|(_, old)| delta < old)
                {
                    best = Some((target, delta));
                }
                continue;
            }
            if script.size >= line.size * 0.87 || delta > 0.62 * line.size {
                continue;
            }
            let mut overlap: f64 = 0.0;
            let mut gap = f64::INFINITY;
            for glyph in line.glyphs.iter().filter(|glyph| visible(glyph)) {
                let distance = (script.start - glyph.end).max(glyph.start - script.end);
                overlap = overlap.max(-distance);
                gap = gap.min(distance.abs());
            }
            if overlap > 0.4 * script.size
                || (script.baseline > line.baseline && overlap > 0.35 * (script.end - script.start))
            {
                continue;
            }
            if gap <= line.size * 0.42 {
                let score = delta + gap * 1.5;
                if best.is_none_or(|(_, old)| score < old) {
                    best = Some((target, score));
                }
            }
        }
        targets[index] = best.map(|(index, _)| index);
    }
    for index in 0..lines.len() {
        if let Some(mut target) = targets[index] {
            while let Some(parent) = targets[target] {
                target = parent;
            }
            let glyphs = std::mem::take(&mut lines[index].glyphs);
            lines[target].glyphs.extend(glyphs);
            lines[target].update_bounds();
        }
    }
    lines.retain(|line| !line.glyphs.is_empty());
}

fn accent(text: &str) -> Option<char> {
    Some(match text {
        "`" | "ˋ" => '\u{300}',
        "´" | "ˊ" => '\u{301}',
        "^" | "ˆ" => '\u{302}',
        "~" | "˜" => '\u{303}',
        "¯" | "ˉ" => '\u{304}',
        "˘" => '\u{306}',
        "˙" => '\u{307}',
        "¨" => '\u{308}',
        "˚" | "°" => '\u{30a}',
        "˝" => '\u{30b}',
        "ˇ" => '\u{30c}',
        "¸" => '\u{327}',
        "˛" => '\u{328}',
        _ => return None,
    })
}

fn accent_distance(mark: &Glyph, letter: &Glyph) -> Option<f64> {
    let center = (mark.start + mark.end) / 2.0;
    let tolerance = 0.1 * letter.size;
    let overlap = mark.end.min(letter.end) - mark.start.max(letter.start);
    if mark.size < 0.65 * letter.size
        || mark.size > 1.5 * letter.size
        || center < letter.start - tolerance
        || center > letter.end + tolerance
        || overlap <= 0.24 * (mark.end - mark.start).min(letter.end - letter.start)
        || letter.text.as_str().chars().count() != 1
        || !letter.text.as_str().chars().all(char::is_alphabetic)
    {
        return None;
    }
    Some((center - (letter.start + letter.end) / 2.0).abs())
}

fn compose_accents(glyphs: &mut Vec<Glyph>) {
    for index in 0..glyphs.len() {
        let Some(mark) = accent(glyphs[index].text.as_str()) else {
            continue;
        };
        let start = glyphs[index].start;
        let radius = glyphs[index].size * 1.3;
        let first = glyphs[..index].partition_point(|glyph| glyph.start < start - radius);
        let nearest = glyphs
            .iter()
            .enumerate()
            .skip(first)
            .take_while(|(_, glyph)| glyph.start <= start + radius)
            .filter_map(|(idx, letter)| Some((idx, accent_distance(&glyphs[index], letter)?)))
            .min_by(|(_, a), (_, b)| a.total_cmp(b));
        if let Some((target, _)) = nearest {
            let bbox = glyphs[index].bbox;
            glyphs[index].text = "".into();
            let letter = &mut glyphs[target];
            let mut text = match letter.text.as_str() {
                "ı" => "i",
                "ȷ" => "j",
                text => text,
            }
            .to_owned();
            text.push(mark);
            letter.text = text.as_str().into();
            letter.bbox = letter.bbox.union(&bbox);
        }
    }
    glyphs.retain(|glyph| !glyph.text.as_str().is_empty());
}

fn main_area(lines: &[Line], page_height: f64) -> Option<BoundingBox> {
    let mut body = lines
        .iter()
        .filter(|line| {
            line.bbox.width > 2.5 * line.size
                && line.bbox.y2() >= 0.045 * page_height
                && line.bbox.y <= 0.955 * page_height
        })
        .map(|line| line.bbox);
    body.next()
        .map(|first| body.fold(first, |area, bbox| area.union(&bbox)))
        .or_else(|| {
            lines
                .iter()
                .map(|line| line.bbox)
                .reduce(|a, b| a.union(&b))
        })
}

fn top_down(a: &Line, b: &Line) -> Ordering {
    a.baseline
        .total_cmp(&b.baseline)
        .then_with(|| a.start.total_cmp(&b.start))
}

fn xy_cut(
    mut lines: Vec<Line>,
    regions: &mut Vec<Vec<Line>>,
    graphics: &[GraphicObject],
    depth: usize,
) {
    if lines.is_empty() {
        return;
    }
    let original_len = lines.len();
    let Some(cut) = (depth < 12).then(|| gutter(&lines)).flatten() else {
        lines.sort_by(top_down);
        regions.push(join_fragments(lines));
        return;
    };

    if let Some(y) = graphic_separator(&lines, graphics, cut).or_else(|| {
        (depth == 0)
            .then(|| header_separator(&lines, cut))
            .flatten()
    }) {
        let (above, below): (Vec<_>, Vec<_>) =
            lines.into_iter().partition(|line| line.baseline <= y);
        xy_cut(above, regions, graphics, depth + 1);
        xy_cut(below, regions, graphics, depth + 1);
        return;
    }
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut spanning = Vec::new();
    for line in lines {
        if line.bbox.x2() <= cut {
            left.push(line);
        } else if line.bbox.x >= cut {
            right.push(line);
        } else if line.bbox.width < 3.0 * line.size {
            if line.bbox.x + line.bbox.width / 2.0 < cut {
                left.push(line);
            } else {
                right.push(line);
            }
        } else {
            spanning.push(line);
        }
    }

    spanning.sort_by(top_down);
    // A band of lines crossing the gutter divides the reading order into
    // separate column runs. Lines alongside a span belong to the same band.
    let mut spans = spanning.into_iter().peekable();
    while let Some(span) = spans.next() {
        let mut band_top = span.bbox.y;
        let mut band_bottom = span.bbox.y2();
        let mut band = vec![span];
        while spans
            .peek()
            .is_some_and(|next| next.bbox.y <= band_bottom + 0.6 * next.size)
        {
            let next = spans.next().expect("span was peeked");
            band_top = band_top.min(next.bbox.y);
            band_bottom = band_bottom.max(next.bbox.y2());
            band.push(next);
        }
        let mut take_band = |side: &mut Vec<Line>| {
            let mut above = Vec::new();
            let mut below = Vec::new();
            for line in side.drain(..) {
                let center = line.bbox.y + line.bbox.height / 2.0;
                if center < band_top {
                    above.push(line);
                } else if center <= band_bottom {
                    band.push(line);
                } else {
                    below.push(line);
                }
            }
            *side = below;
            above
        };
        let above_left = take_band(&mut left);
        let above_right = take_band(&mut right);
        xy_cut(above_left, regions, graphics, depth + 1);
        xy_cut(above_right, regions, graphics, depth + 1);
        if depth + 1 < 12
            && band.len() < original_len
            && gutter(&band).is_some_and(|sub_cut| (sub_cut - cut).abs() > 12.0)
        {
            xy_cut(band, regions, graphics, depth + 1);
        } else {
            band.sort_by(top_down);
            regions.push(join_fragments(band));
        }
    }
    xy_cut(left, regions, graphics, depth + 1);
    xy_cut(right, regions, graphics, depth + 1);
}

// Author affiliations may occupy parallel columns above the body. A shared
// blank band at the start of the page separates those columns from the article.
fn header_separator(lines: &[Line], cut: f64) -> Option<f64> {
    let mut rows: Vec<_> = lines.iter().collect();
    rows.sort_by(|a, b| a.bbox.y.total_cmp(&b.bbox.y));
    let top = rows.first()?.bbox.y;
    let bottom = rows.iter().map(|line| line.bbox.y2()).fold(top, f64::max);
    let title = rows
        .iter()
        .take_while(|line| line.bbox.y < top + 0.22 * (bottom - top))
        .find(|line| line.bbox.x < cut && line.bbox.x2() > cut)?;
    let side = |line: &Line| {
        if line.bbox.x2() <= cut {
            1
        } else if line.bbox.x >= cut {
            2
        } else {
            0
        }
    };
    let mut edge = title.bbox.y2();
    let mut seen = 0;
    for (index, line) in rows.iter().enumerate() {
        let start = line.bbox.y;
        if start > top + 0.50 * (bottom - top) {
            break;
        }
        if start < title.bbox.y || line.bbox.y2() <= title.bbox.y2() {
            continue;
        }
        let size = line.size;
        if seen == 3 && start - edge > (0.8 * size).max(8.0) {
            let nearby = rows[index..]
                .iter()
                .take_while(|other| other.bbox.y < start + 3.5 * size)
                .fold(0, |sides, line| sides | side(line));
            let prior = rows[..index]
                .iter()
                .rev()
                .take_while(|other| other.bbox.y >= edge - 6.0 * size);
            let (width, count) = prior
                .filter(|line| side(line) != 0)
                .fold((0.0, 0), |(sum, count), line| {
                    (sum + line.bbox.width, count + 1)
                });
            let after_width = rows[index..]
                .iter()
                .take(8)
                .map(|line| line.bbox.width)
                .fold(0.0, f64::max);
            if nearby == 3 && count > 0 && width / f64::from(count) < 0.84 * after_width {
                return Some((edge + start) / 2.0);
            }
        }
        seen |= side(line);
        edge = edge.max(line.bbox.y2());
    }
    None
}

fn graphic_separator(lines: &[Line], graphics: &[GraphicObject], cut: f64) -> Option<f64> {
    let x0 = lines
        .iter()
        .map(|line| line.bbox.x)
        .fold(f64::INFINITY, f64::min);
    let x1 = lines
        .iter()
        .map(|line| line.bbox.x2())
        .fold(f64::NEG_INFINITY, f64::max);
    for graphic in graphics {
        let bbox = graphic.bbox;
        if !matches!(graphic.kind, GraphicKind::Image | GraphicKind::VectorBox)
            || bbox.width < 0.66 * (x1 - x0)
            || bbox.height < 32.0
            || bbox.x >= cut
            || bbox.x2() <= cut
        {
            continue;
        }
        let mut above = 0;
        let mut below = 0;
        let mut overlap = 0;
        for line in lines {
            if line.baseline < bbox.y {
                above += 1;
            } else if line.baseline > bbox.y2() {
                below += 1;
            } else {
                overlap += 1;
            }
        }
        if above >= 4 && below >= 4 && overlap <= 3 {
            // Keep figure labels with the text above the figure.
            return Some(bbox.y2());
        }
    }
    None
}

fn gutter(lines: &[Line]) -> Option<f64> {
    if lines.len() < 4 {
        return None;
    }
    let x0 = lines
        .iter()
        .map(|line| line.bbox.x)
        .fold(f64::INFINITY, f64::min);
    let x1 = lines
        .iter()
        .map(|line| line.bbox.x2())
        .fold(f64::NEG_INFINITY, f64::max);
    let mut font_sizes: Vec<_> = lines.iter().map(|line| line.size).collect();
    font_sizes.sort_unstable_by(f64::total_cmp);
    let size = font_sizes[font_sizes.len() / 2];
    let width = x1 - x0;
    if width < 12.0 * size {
        return None;
    }

    #[derive(Default)]
    struct Side {
        count: usize,
        top: f64,
        bottom: f64,
    }
    impl Side {
        fn add(&mut self, line: &Line) {
            self.count += 1;
            self.top = self.top.min(line.bbox.y);
            self.bottom = self.bottom.max(line.bbox.y2());
        }
        fn new() -> Self {
            Self {
                count: 0,
                top: f64::INFINITY,
                bottom: f64::NEG_INFINITY,
            }
        }
    }

    let mut best: Option<(f64, f64)> = None;
    for sample in 12..85 {
        let x = x0 + width * f64::from(sample) / 96.0;
        let mut left = Side::new();
        let mut right = Side::new();
        let mut left_edge = f64::INFINITY;
        let mut right_edge = f64::INFINITY;
        let mut cross = 0;
        for line in lines {
            if line.bbox.x2() <= x {
                left_edge = left_edge.min(x - line.bbox.x2());
                if line.bbox.width >= 4.0 * size {
                    left.add(line);
                }
            } else if line.bbox.x >= x {
                right_edge = right_edge.min(line.bbox.x - x);
                if line.bbox.width >= 4.0 * size {
                    right.add(line);
                }
            } else {
                cross += 1;
            }
        }
        let count = left.count.min(right.count);
        let overlap_top = left.top.max(right.top);
        let overlap_bottom = left.bottom.min(right.bottom);
        let overlap = overlap_bottom - overlap_top;
        let gap = left_edge + right_edge;
        if count < 2 || overlap < size || gap < (0.55 * size).max(4.0) {
            continue;
        }
        let crossing_columns = if cross == 0 {
            0
        } else {
            lines
                .iter()
                .filter(|line| {
                    line.bbox.x < x
                        && line.bbox.x2() > x
                        && (overlap_top..=overlap_bottom).contains(&line.baseline)
                })
                .count()
        };
        if crossing_columns > count {
            continue;
        }
        let center = x + (right_edge - left_edge) / 2.0;
        // Full-width material outside the shared vertical run is often a title or abstract.
        let score = 3.0 * count as f64
            - 2.0 * crossing_columns as f64
            - 0.15 * (cross - crossing_columns) as f64
            + (gap / size).min(4.0)
            + (overlap / size).min(30.0) * 0.1;
        if best.is_none_or(|(_, old)| score > old) {
            best = Some((center, score));
        }
    }
    best.map(|(cut, _)| cut)
}

fn join_fragments(lines: Vec<Line>) -> Vec<Line> {
    let mut joined: Vec<Line> = Vec::new();
    for mut line in lines {
        if let Some(last) = joined.last_mut()
            && (last.baseline - line.baseline).abs() < 0.24 * last.size.min(line.size)
            && line.start >= last.start
            && line.start - last.end <= 5.0 * last.size
        {
            last.glyphs.append(&mut line.glyphs);
            last.update_bounds();
            last.sort_glyphs();
        } else {
            joined.push(line);
        }
    }
    joined
}

fn append_region(doc: &mut LayoutDocument, page: usize, lines: Vec<Line>, fonts: &[Font]) {
    let mut previous: Option<Line> = None;
    let mut block: Option<LayoutBlock> = None;
    for mut line in lines {
        let starts_block = previous.as_ref().is_none_or(|last| separate(last, &line));
        if starts_block {
            if let Some(completed) = block.take() {
                doc.blocks.push(completed);
            }
            block = Some(LayoutBlock {
                page,
                bbox: line.bbox,
                block_idx: doc.blocks.len(),
                token_range: doc.tokens.len()..doc.tokens.len(),
                line_range: doc.lines.len()..doc.lines.len(),
            });
        }
        let block = block.as_mut().expect("region has a current block");
        let line_idx = doc.lines.len();
        let token_start = doc.tokens.len();
        let glyphs = std::mem::take(&mut line.glyphs);
        tokenize_line(
            &line,
            glyphs,
            fonts,
            block.block_idx,
            line_idx,
            &mut doc.tokens,
        );
        let token_end = doc.tokens.len();
        if token_end == token_start {
            continue;
        }
        doc.lines.push(LayoutLine {
            page,
            bbox: line.bbox,
            token_range: token_start..token_end,
            block_idx: block.block_idx,
        });
        block.bbox = block.bbox.union(&line.bbox);
        block.line_range.end = doc.lines.len();
        block.token_range.end = token_end;
        previous = Some(line);
    }
    if let Some(block) = block.filter(|block| !block.token_range.is_empty()) {
        doc.blocks.push(block);
    }
}

fn separate(previous: &Line, line: &Line) -> bool {
    let size = previous.size.max(line.size);
    let gap = line.baseline - previous.baseline;
    let indent = (line.start - previous.start).abs();
    let centered = (line.start + line.end - previous.start - previous.end).abs() < 0.6 * size
        && line.style == previous.style
        && size > 12.0;
    gap <= 0.0
        || gap > size * 1.63
        || (!centered && indent > size * 3.6)
        || (line.size - previous.size).abs() > size * 0.14
        || (line.style != previous.style && (gap > size * 1.12 || indent > size * 0.8))
        || (!centered
            && line.start - previous.start > size * 0.82
            && previous.end - previous.start > 6.0 * size)
        || (!centered && indent > size * 1.4 && previous.end < line.end - size * 3.0)
}

fn tokenize_line(
    line: &Line,
    glyphs: Vec<Glyph>,
    fonts: &[Font],
    block_idx: usize,
    line_idx: usize,
    tokens: &mut Vec<LayoutToken>,
) {
    // Some PDF generators (notably Springer Nature) encode word spaces by setting a
    // large positive Tc and pulling every intra-word glyph pair back via TJ. When
    // positive Tc is cancelled by TJ pullbacks on the line, visual word gaps exist
    // precisely where Tc is left uncancelled, so adding positive char_space to glyph
    // end would erase those spaces.
    let tc_cancelled = glyphs.windows(2).any(|pair| {
        let a = &pair[0];
        let b = &pair[1];
        visible(a)
            && visible(b)
            && a.char_space > 0.08 * line.size
            && (b.start - a.end) < 0.35 * a.char_space
    });
    // Detect letter-spaced headings (e.g. tracked uppercase titles or wide monospace)
    // where almost every adjacent glyph pair has a deliberate positive tracking gap.
    let mut intra_gaps: Vec<f64> = glyphs
        .windows(2)
        .filter_map(|pair| {
            let a = &pair[0];
            let b = &pair[1];
            if !visible(a) || !visible(b) {
                return None;
            }
            let cs = if tc_cancelled && a.char_space > 0.0 {
                0.0
            } else {
                a.char_space
            };
            let gap = b.start - (a.end + cs);
            (gap >= -0.05 * line.size && gap < 0.35 * line.size).then_some(gap.max(0.0))
        })
        .collect();
    let tracking_floor = if intra_gaps.len() >= 6 {
        intra_gaps.sort_unstable_by(f64::total_cmp);
        let median = intra_gaps[intra_gaps.len() / 2];
        if median > 0.06 * line.size {
            median * 1.65
        } else {
            0.0
        }
    } else {
        0.0
    };

    let mut current: Option<(LayoutToken, usize)> = None;
    let mut forced_space = false;
    let mut previous: Option<(f64, f64)> = None;
    let first_token = tokens.len();
    for glyph in glyphs {
        let font = &fonts[glyph.font];
        let delta = line.baseline - glyph.baseline;
        let small = glyph.size < line.size * 0.91;
        let raised = glyph.rise.abs() > 0.12 * glyph.size;
        let superscript = (raised && glyph.rise > 0.0) || (small && delta > 0.16 * line.size);
        let subscript =
            !superscript && ((raised && glyph.rise < 0.0) || (small && delta < -0.12 * line.size));
        if let Some((end, space)) = previous {
            let max_gap = (0.135 * line.size).max(0.50);
            let base = (0.30 * space.min(glyph.space_width)).clamp(0.42, max_gap);
            let threshold = base.max(tracking_floor);
            forced_space |= glyph.start - end > threshold;
        }
        let chars = glyph.text.as_str().chars().count();
        for (index, character) in glyph.text.as_str().chars().enumerate() {
            let whitespace = character.is_whitespace() || character == '\u{200c}';
            let punct = punctuation(character);
            let compatible = current.as_ref().is_some_and(|(token, font_id)| {
                *font_id == glyph.font
                    && (token.font_size - glyph.size).abs() <= 0.1
                    && token.superscript == superscript
                    && token.subscript == subscript
                    && token.color == glyph.color
                    && token.rotated == glyph.rotated
            });
            if (whitespace || punct || forced_space || !compatible)
                && let Some((token, _)) = current.take()
            {
                tokens.push(token);
            }
            if whitespace {
                forced_space = true;
                continue;
            }
            if forced_space && tokens.len() > first_token {
                tokens
                    .last_mut()
                    .expect("line has a previous token")
                    .space_after = true;
            }
            forced_space = false;
            let bbox = glyph_fragment(glyph.bbox, glyph.direction, index, chars);
            if let Some((token, _)) = &mut current {
                token.text.push(character);
                token.bbox = token.bbox.union(&bbox);
            } else {
                let token = LayoutToken {
                    text: character.to_string(),
                    page: bbox.page,
                    bbox,
                    font_name: font.name.clone(),
                    font_size: glyph.size,
                    bold: font.bold,
                    italic: font.italic,
                    subscript,
                    superscript,
                    color: glyph.color,
                    rotated: glyph.rotated,
                    space_after: false,
                    new_line_after: false,
                    block_idx,
                    line_idx,
                };
                if punct {
                    tokens.push(token);
                } else {
                    current = Some((token, glyph.font));
                }
            }
        }
        let effective_char_space = if tc_cancelled && glyph.char_space > 0.0 {
            0.0
        } else {
            glyph.char_space
        };
        let end = glyph.end + effective_char_space;
        let end = previous.map_or(end, |(previous, _)| previous.max(end));
        previous = Some((end, glyph.space_width));
    }
    if let Some((token, _)) = current {
        tokens.push(token);
    }
    if let Some(last) = tokens
        .get_mut(first_token..)
        .and_then(|line| line.last_mut())
    {
        last.new_line_after = true;
    }
    for token in &mut tokens[first_token..] {
        if !token.text.is_ascii() && !unicode_normalization::is_nfc(&token.text) {
            token.text = token.text.nfc().collect();
        }
    }
}

fn punctuation(character: char) -> bool {
    "(（[{}•*,:;?.!/)）-−–—‐«»„\"“”‘’'`$#@]♦♥♣♠。、，・…".contains(character)
}

fn glyph_fragment(
    mut bbox: BoundingBox,
    direction: Direction,
    index: usize,
    total: usize,
) -> BoundingBox {
    if total <= 1 {
        return bbox;
    }
    let (horizontal, reversed) = direction.fragment_axis();
    let index = if reversed { total - index - 1 } else { index };
    if horizontal {
        bbox.width /= total as f64;
        bbox.x += bbox.width * index as f64;
    } else {
        bbox.height /= total as f64;
        bbox.y += bbox.height * index as f64;
    }
    bbox
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xy_cut_orders_columns_around_spanning_lines() {
        let line = |text: &str, x, y, width| {
            let glyph = Glyph {
                text: text.into(),
                bbox: BoundingBox::new(1, x, y, width, 9.0),
                font: 0,
                size: 10.0,
                color: (0, 0, 0),
                direction: Direction::Right,
                rotated: false,
                start: x,
                end: x + width,
                baseline: y + 7.0,
                rise: 0.0,
                space_width: 2.5,
                char_space: 0.0,
            };
            Line::new(vec![glyph], y + 7.0, 10.0)
        };
        let lines = vec![
            line("r1", 310.0, 44.0, 230.0),
            line("l2", 55.0, 57.0, 230.0),
            line("title", 130.0, 14.0, 340.0),
            line("r2", 310.0, 57.0, 230.0),
            line("l1", 55.0, 44.0, 230.0),
            line("caption", 75.0, 95.0, 445.0),
            line("r3", 310.0, 130.0, 180.0),
            line("l4", 55.0, 143.0, 230.0),
            line("r4", 310.0, 143.0, 230.0),
            line("l3", 55.0, 130.0, 230.0),
        ];
        let mut regions = Vec::new();
        xy_cut(lines, &mut regions, &[], 0);
        let order: Vec<_> = regions
            .iter()
            .flatten()
            .map(|line| line.glyphs[0].text.as_str())
            .collect();
        assert_eq!(
            order,
            [
                "title", "l1", "l2", "r1", "r2", "caption", "l3", "l4", "r3", "r4"
            ]
        );
    }

    #[test]
    fn gutter_keeps_narrow_hanging_indent_labels_in_right_column() {
        let line = |text: &str, x, y, width| {
            let glyph = Glyph {
                text: text.into(),
                bbox: BoundingBox::new(1, x, y, width, 7.8),
                font: 0,
                size: 8.0,
                color: (0, 0, 0),
                direction: Direction::Right,
                rotated: false,
                start: x,
                end: x + width,
                baseline: y + 6.0,
                rise: 0.0,
                space_width: 2.0,
                char_space: 0.0,
            };
            Line::new(vec![glyph], y + 6.0, 8.0)
        };
        // Hanging-indent references: left col ends at 294.8, right col labels sit at
        // 306.1..317.8 (< 4 * size) while right continuation lines start at 328.1.
        let lines = vec![
            line("46.", 39.7, 49.0, 11.7),
            line("left1", 61.6, 49.0, 233.2),
            line("left2", 61.6, 60.0, 233.2),
            line("47.", 39.7, 71.0, 11.7),
            line("left3", 61.6, 71.0, 233.2),
            line("68.", 306.1, 49.0, 11.7),
            line("right1", 328.1, 49.0, 220.0),
            line("right2", 328.1, 60.0, 220.0),
            line("69.", 306.1, 71.0, 11.7),
            line("right3", 328.1, 71.0, 220.0),
        ];
        let mut regions = Vec::new();
        xy_cut(lines, &mut regions, &[], 0);
        // Left column must finish completely before right column begins.
        let texts: Vec<String> = regions
            .iter()
            .flatten()
            .map(|l| {
                l.glyphs
                    .iter()
                    .map(|g| g.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        assert_eq!(
            texts,
            ["46. left1", "left2", "47. left3", "68. right1", "right2", "69. right3"]
        );
    }

    #[test]
    fn tokenize_line_recovers_spaces_when_positive_tc_is_cancelled_by_tj() {
        let doc = lopdf::Document::new();
        let dict = lopdf::Dictionary::new();
        let fonts = vec![Font::new(&doc, &dict, b"F1")];
        let mk = |ch: &str, start: f64, width: f64, char_space: f64| Glyph {
            text: ch.into(),
            bbox: BoundingBox::new(1, start, 100.0, width, 9.0),
            font: 0,
            size: 9.2,
            color: (0, 0, 0),
            direction: Direction::Right,
            rotated: false,
            start,
            end: start + width,
            baseline: 107.0,
            rise: 0.0,
            space_width: 2.3,
            char_space,
        };
        // Springer Nature pattern: Tc = +2.37 pt, pulled back by TJ inside words,
        // left uncancelled between "g" and "H" to produce a 2.37 pt visual word space.
        let glyphs = vec![
            mk("Y", 10.0, 5.0, 2.37),
            mk("u", 15.0, 5.0, 2.37),
            mk("g", 20.0, 5.0, 2.37),
            mk("H", 27.37, 6.0, 2.37),
            mk("u", 33.37, 5.0, 2.37),
        ];
        let line = Line::new(vec![mk("Y", 10.0, 5.0, 2.37)], 107.0, 9.2);
        let mut tokens = Vec::new();
        tokenize_line(&line, glyphs, &fonts, 0, 0, &mut tokens);
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].text, "Yug");
        assert!(tokens[0].space_after);
        assert_eq!(tokens[1].text, "Hu");
    }
}
