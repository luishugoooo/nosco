use std::{mem, ops::Range};

use crate::{CrfError, CrfModel, ModelType, is_c_space, pattern::c_bytes};

/// Reusable work buffers for a single decoder. Use one per concurrent caller.
#[derive(Debug, Default)]
pub struct DecodeScratch {
    potentials: Potentials,
    current: Vec<f64>,
    previous: Vec<f64>,
    back: Vec<u16>,
    alpha: Vec<f64>,
    column_ranges: Vec<Range<usize>>,
    row_offsets: Vec<usize>,
}

impl DecodeScratch {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Debug, PartialEq)]
pub struct DecodeResult {
    pub label_ids: Vec<usize>,
    /// Total additive potential for CRF/MaxEnt Viterbi, or the product of
    /// conditional probabilities (MEMM) or token marginals (CRF posterior).
    pub sequence_score: f64,
    /// The selected local potential or probability at each position.
    pub token_scores: Vec<f64>,
}

impl DecodeResult {
    fn empty(product: bool) -> Self {
        Self {
            label_ids: Vec::new(),
            sequence_score: if product { 1.0 } else { 0.0 },
            token_scores: Vec::new(),
        }
    }
}

impl CrfModel {
    pub fn tag<S: AsRef<str>>(
        &self,
        rows: &[Vec<S>],
        scratch: &mut DecodeScratch,
    ) -> Result<Vec<usize>, CrfError> {
        Ok(self.viterbi(rows, scratch, false)?.label_ids)
    }

    pub fn tag_slices(
        &self,
        rows: &[&[&str]],
        scratch: &mut DecodeScratch,
    ) -> Result<Vec<usize>, CrfError> {
        Ok(self.viterbi(rows, scratch, false)?.label_ids)
    }

    /// Tag one sequence, splitting each supplied row on C-locale whitespace.
    pub fn tag_lines(
        &self,
        lines: &[&str],
        scratch: &mut DecodeScratch,
    ) -> Result<Vec<usize>, CrfError> {
        let mut columns = mem::take(&mut scratch.column_ranges);
        let mut offsets = mem::take(&mut scratch.row_offsets);
        columns.clear();
        offsets.clear();
        offsets.push(0);
        for line in lines {
            let bytes = c_bytes(line);
            let mut start = 0;
            for (i, byte) in bytes.iter().copied().enumerate() {
                if is_c_space(byte) {
                    if start < i {
                        columns.push(start..i);
                    }
                    start = i + 1;
                }
            }
            let end = bytes.len();
            if start < end {
                columns.push(start..end);
            }
            offsets.push(columns.len());
        }
        let rows = LineRows {
            lines,
            columns: &columns,
            offsets: &offsets,
        };
        let result = self
            .viterbi(&rows, scratch, false)
            .map(|result| result.label_ids);
        scratch.column_ranges = columns;
        scratch.row_offsets = offsets;
        result
    }

    pub fn tag_with_scores(
        &self,
        rows: &[&[&str]],
        scratch: &mut DecodeScratch,
    ) -> Result<DecodeResult, CrfError> {
        self.viterbi(rows, scratch, true)
    }

    /// Decode by scaled forward-backward marginals. Wapiti uses conditional
    /// Viterbi scores for MEMM models even when posterior decoding is enabled.
    pub fn tag_posterior(
        &self,
        rows: &[&[&str]],
        scratch: &mut DecodeScratch,
    ) -> Result<DecodeResult, CrfError> {
        if self.model_type == ModelType::Memm {
            return self.tag_with_scores(rows, scratch);
        }
        let t = rows.len();
        let y = self.num_labels();
        if t == 0 {
            return Ok(DecodeResult::empty(true));
        }
        scratch.prepare(t, y)?;
        let DecodeScratch {
            potentials,
            alpha,
            current,
            previous,
            back,
            ..
        } = scratch;
        resize(alpha, matrix_len(t, y)?)?;

        potentials.fill(self, rows, 0)?;
        potentials.exponentiate(0, false);
        alpha[..y].copy_from_slice(&potentials.uni);
        normalize(&mut alpha[..y]);
        for at in 1..t {
            potentials.fill(self, rows, at)?;
            potentials.exponentiate(at, false);
            let (past, future) = alpha.split_at_mut(at * y);
            let before = &past[past.len() - y..];
            let after = &mut future[..y];
            after.fill(0.0);
            for (&weight, transitions) in before.iter().zip(potentials.bi.chunks_exact(y)) {
                for (sum, &transition) in after.iter_mut().zip(transitions) {
                    *sum += weight * transition;
                }
            }
            normalize(after);
        }

        current.fill(1.0 / y as f64);
        for at in (0..t).rev() {
            // Overwrite each forward row with its marginals after its last use.
            let marginal = &mut alpha[at * y..(at + 1) * y];
            let z: f64 = marginal
                .iter_mut()
                .zip(&*current)
                .map(|(a, b)| {
                    *a *= *b;
                    *a
                })
                .sum();
            let scale = 1.0 / z;
            for value in marginal {
                *value *= scale;
            }
            if at > 0 {
                potentials.fill(self, rows, at)?;
                potentials.exponentiate(at, false);
                for (score, transitions) in previous.iter_mut().zip(potentials.bi.chunks_exact(y)) {
                    *score = current.iter().zip(transitions).map(|(b, p)| b * p).sum();
                }
                normalize(previous);
                mem::swap(current, previous);
            }
        }

        current.copy_from_slice(&alpha[..y]);
        for (at, marginal) in alpha.chunks_exact(y).enumerate().skip(1) {
            mem::swap(current, previous);
            let best = argmax(previous);
            let parents = &mut back[at * y..(at + 1) * y];
            for ((score, parent), &probability) in current.iter_mut().zip(parents).zip(marginal) {
                *score = previous[best] * probability;
                // Match strict-greater Viterbi tie-breaking, including when
                // products round to the same value or underflow to zero.
                let earliest = previous[..best]
                    .iter()
                    .position(|p| p * probability == *score);
                *parent = earliest.unwrap_or(best) as u16;
            }
        }
        let best = argmax(current);
        let label_ids = backtrack(back, t, y, best);
        let token_scores = label_ids
            .iter()
            .enumerate()
            .map(|(at, &label)| alpha[at * y + label])
            .collect();
        Ok(DecodeResult {
            label_ids,
            sequence_score: current[best],
            token_scores,
        })
    }

    fn viterbi(
        &self,
        rows: &(impl Rows + ?Sized),
        scratch: &mut DecodeScratch,
        scored: bool,
    ) -> Result<DecodeResult, CrfError> {
        let t = rows.len();
        let y = self.num_labels();
        let memm = self.model_type == ModelType::Memm;
        if t == 0 {
            return Ok(DecodeResult::empty(memm));
        }
        scratch.prepare(t, y)?;
        let DecodeScratch {
            potentials,
            current,
            previous,
            back,
            ..
        } = scratch;
        potentials.fill(self, rows, 0)?;
        if memm {
            potentials.exponentiate(0, true);
        }
        current.copy_from_slice(&potentials.uni);
        for at in 1..t {
            potentials.fill(self, rows, at)?;
            mem::swap(current, previous);
            let parents = &mut back[at * y..(at + 1) * y];
            if memm {
                potentials.exponentiate(at, true);
                advance::<true>(previous, &potentials.uni, &potentials.bi, current, parents);
            } else {
                advance::<false>(previous, &potentials.uni, &potentials.bi, current, parents);
            }
        }

        let best = argmax(current);
        let sequence_score = current[best];
        let label_ids = backtrack(back, t, y, best);
        let mut token_scores = Vec::new();
        if scored {
            token_scores.reserve(t);
            for (at, &label) in label_ids.iter().enumerate() {
                potentials.fill(self, rows, at)?;
                if memm {
                    potentials.exponentiate(at, true);
                }
                let score = if at == 0 {
                    potentials.uni[label]
                } else {
                    let transition = potentials.bi[label_ids[at - 1] * y + label];
                    if memm {
                        transition
                    } else {
                        potentials.uni[label] + transition
                    }
                };
                token_scores.push(score);
            }
        }
        Ok(DecodeResult {
            label_ids,
            sequence_score,
            token_scores,
        })
    }
}

fn advance<const PRODUCT: bool>(
    previous: &[f64],
    uni: &[f64],
    bi: &[f64],
    current: &mut [f64],
    back: &mut [u16],
) {
    current.fill(f64::NEG_INFINITY);
    back.fill(0);
    for (prev, (&prefix, transitions)) in
        previous.iter().zip(bi.chunks_exact(uni.len())).enumerate()
    {
        // Previous-label order preserves Wapiti's first-wins tie-breaking.
        for (((best, parent), &unigram), &bigram) in
            current.iter_mut().zip(&mut *back).zip(uni).zip(transitions)
        {
            let score = if PRODUCT {
                prefix * bigram
            } else {
                prefix + (unigram + bigram)
            };
            if score > *best {
                *best = score;
                *parent = prev as u16;
            }
        }
    }
}

fn argmax(scores: &[f64]) -> usize {
    let mut best = 0;
    for (id, &score) in scores.iter().enumerate().skip(1) {
        if score > scores[best] {
            best = id;
        }
    }
    best
}

fn backtrack(back: &[u16], t: usize, y: usize, mut label: usize) -> Vec<usize> {
    let mut labels = vec![0; t];
    for at in (1..t).rev() {
        labels[at] = label;
        label = usize::from(back[at * y + label]);
    }
    labels[0] = label;
    labels
}

fn matrix_len(rows: usize, columns: usize) -> Result<usize, CrfError> {
    rows.checked_mul(columns).ok_or(CrfError::TooLarge)
}

fn resize<T: Default + Copy>(buffer: &mut Vec<T>, len: usize) -> Result<(), CrfError> {
    if len > buffer.len() {
        buffer
            .try_reserve(len - buffer.len())
            .map_err(|_| CrfError::TooLarge)?;
    }
    buffer.resize(len, T::default());
    Ok(())
}

impl DecodeScratch {
    fn prepare(&mut self, t: usize, y: usize) -> Result<(), CrfError> {
        resize(&mut self.potentials.uni, y)?;
        if t > 1 {
            resize(&mut self.potentials.bi, matrix_len(y, y)?)?;
        }
        resize(&mut self.current, y)?;
        resize(&mut self.previous, y)?;
        resize(&mut self.back, matrix_len(t, y)?)
    }
}

fn normalize(values: &mut [f64]) {
    let scale = 1.0 / values.iter().sum::<f64>();
    for value in values {
        *value *= scale;
    }
}

#[derive(Debug, Default)]
struct Potentials {
    uni: Vec<f64>,
    bi: Vec<f64>,
    observation: Vec<u8>,
}

impl Potentials {
    fn fill(
        &mut self,
        model: &CrfModel,
        rows: &(impl Rows + ?Sized),
        at: usize,
    ) -> Result<(), CrfError> {
        self.uni.fill(0.0);
        if at > 0 {
            self.bi.fill(0.0);
        }
        self.observation.clear();
        if model.autouni {
            self.observation.push(b'u');
        }
        if model.patterns.is_empty() {
            for col in 0..rows.width(at) {
                let token = c_bytes(rows.get(at, col).unwrap());
                let key = if model.autouni {
                    self.observation.truncate(1);
                    self.observation.extend_from_slice(token);
                    self.observation.as_slice()
                } else {
                    if !matches!(token.first(), Some(b'u' | b'b' | b'*')) {
                        return Err(CrfError::InvalidObservation {
                            row: at,
                            column: col,
                        });
                    }
                    token
                };
                if let Some(id) = model.observations.find(key) {
                    self.add(model, id, at > 0);
                }
            }
        } else {
            for pattern in &model.patterns {
                pattern.format(
                    rows.len(),
                    at,
                    usize::from(model.autouni),
                    &mut self.observation,
                    |row, col| rows.get(row, col),
                )?;
                let kind = pattern.as_str().as_bytes()[0];
                if model.autouni && kind == b'b' {
                    continue;
                }
                if let Some(id) = model.observations.find(&self.observation) {
                    self.add(model, id, at > 0);
                }
            }
        }
        Ok(())
    }

    fn add(&mut self, model: &CrfModel, id: usize, transitions: bool) {
        for w in model.unigram.get(id) {
            self.uni[usize::from(w.label)] += w.weight;
        }
        if transitions {
            let y = self.uni.len();
            for w in model.bigram.get(id) {
                self.bi[usize::from(w.prev) * y + usize::from(w.cur)] += w.weight;
            }
        }
    }

    fn exponentiate(&mut self, at: usize, conditional: bool) {
        if at == 0 {
            exponentiate(&mut self.uni, conditional);
            return;
        }
        let y = self.uni.len();
        for row in self.bi.chunks_exact_mut(y) {
            for (transition, unigram) in row.iter_mut().zip(&self.uni) {
                *transition += *unigram;
            }
            if conditional {
                exponentiate(row, true);
            }
        }
        if !conditional {
            exponentiate(&mut self.bi, false);
        }
    }
}

fn exponentiate(scores: &mut [f64], normalize_row: bool) {
    // A common shift preserves the subsequent normalization. Use it for
    // scores outside the range where unshifted exp and summation are safe.
    let largest = scores[argmax(scores)];
    let shift = if !(-650.0..=650.0).contains(&largest) {
        largest
    } else {
        0.0
    };
    for score in &mut *scores {
        *score = (*score - shift).exp();
    }
    if normalize_row {
        let sum = scores.iter().sum::<f64>();
        for score in scores {
            *score /= sum;
        }
    }
}

trait Rows {
    fn len(&self) -> usize;
    fn width(&self, row: usize) -> usize;
    fn get(&self, row: usize, col: usize) -> Option<&str>;
}

impl<S: AsRef<str>> Rows for [Vec<S>] {
    fn len(&self) -> usize {
        <[_]>::len(self)
    }
    fn width(&self, row: usize) -> usize {
        self[row].len()
    }
    fn get(&self, row: usize, col: usize) -> Option<&str> {
        self[row].get(col).map(AsRef::as_ref)
    }
}

impl Rows for [&[&str]] {
    fn len(&self) -> usize {
        <[_]>::len(self)
    }
    fn width(&self, row: usize) -> usize {
        self[row].len()
    }
    fn get(&self, row: usize, col: usize) -> Option<&str> {
        self[row].get(col).copied()
    }
}

struct LineRows<'a> {
    lines: &'a [&'a str],
    columns: &'a [Range<usize>],
    offsets: &'a [usize],
}

impl Rows for LineRows<'_> {
    fn len(&self) -> usize {
        self.lines.len()
    }
    fn width(&self, row: usize) -> usize {
        self.offsets[row + 1] - self.offsets[row]
    }
    fn get(&self, row: usize, col: usize) -> Option<&str> {
        let columns = &self.columns[self.offsets[row]..self.offsets[row + 1]];
        Some(&self.lines[row][columns.get(col)?.clone()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viterbi_uses_one_transition_matrix() {
        let model = CrfModel::from_wapiti_reader(
            &b"#mdl#2#1\n#rdr#0/0\n#qrk#3\n1:A,\n1:B,\n1:C,\n#qrk#1\n3:u:x,\n1=1\n"[..],
        )
        .unwrap();
        let row: &[&str] = &["u:x"];
        let mut scratch = DecodeScratch::new();
        assert_eq!(model.tag_slices(&[row], &mut scratch).unwrap(), [1]);
        assert!(scratch.potentials.bi.is_empty());

        let rows = vec![row; 2_000];
        let labels = model.tag_slices(&rows, &mut scratch).unwrap();
        assert!(labels.iter().all(|&label| label == 1));
        assert_eq!(scratch.potentials.bi.len(), 9);
        assert_eq!(scratch.back.len(), rows.len() * 3);
        assert!(scratch.alpha.is_empty());
    }
}
