//! Row-parallel helpers.
//!
//! Every expensive loop in this crate is a per-pixel map or reduction over a
//! few hundred million samples, with identical work at every pixel. That is
//! the case where a work-stealing scheduler buys nothing over a static split,
//! so these are built on [`std::thread::scope`] rather than pulling in a
//! dependency: chunk by whole rows, one thread per chunk, join.
//!
//! Chunking by *rows* rather than by samples is what keeps callers simple —
//! a closure gets a contiguous slice plus the row index it starts at, so
//! anything that needs `(x, y)` can still compute it.

/// Worker count: the machine's parallelism, or 1 if it cannot be determined.
///
/// Cached because this runs once per parallel region. Deliberately not
/// throttled when several conversions share the machine: `tohdr batch` measured
/// that slower, since a job blocked in ImageIO's serial decode holds a share it
/// is not using.
pub fn threads() -> usize {
    static CORES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CORES.get_or_init(|| {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    })
}

/// Alias for [`threads`], for callers deciding *how many* concurrent jobs to
/// run rather than how to split one.
pub fn available_cores() -> usize {
    threads()
}

/// Rows per chunk for `rows` rows across `threads()` workers, rounded up to a
/// multiple of `granularity`.
///
/// `granularity` exists for the subsampled gain plane: if one output row
/// consumes `subsample` input rows, chunk boundaries must fall on multiples of
/// `subsample` or two threads end up accumulating into the same output bucket.
fn rows_per_chunk(rows: usize, granularity: usize) -> usize {
    let g = granularity.max(1);
    let n = threads().max(1);
    let per = rows.div_ceil(n);
    per.div_ceil(g) * g
}

/// Run `f(start_row, chunk)` over whole-row chunks of `data`, in parallel.
///
/// `f` must be `Sync` because every worker shares it. Falls back to a direct
/// call when there is only one chunk, so small images pay no thread overhead.
pub fn for_each_row_chunk_mut<T, F>(data: &mut [T], row_len: usize, granularity: usize, f: F)
where
    T: Send,
    F: Fn(usize, &mut [T]) + Sync,
{
    if row_len == 0 || data.is_empty() {
        return;
    }
    let rows = data.len() / row_len;
    let per = rows_per_chunk(rows, granularity);
    let chunk_len = per * row_len;
    if chunk_len == 0 || chunk_len >= data.len() {
        f(0, data);
        return;
    }
    std::thread::scope(|s| {
        for (i, chunk) in data.chunks_mut(chunk_len).enumerate() {
            let f = &f;
            s.spawn(move || f(i * per, chunk));
        }
    });
}

/// Map each whole-row chunk of `data` to a value, in parallel, and return the
/// per-chunk results in order. The caller combines them.
///
/// Returning per-chunk partials rather than folding inside keeps the
/// combination explicit, which matters for min/max over floats where the
/// identity element and NaN handling are decisions the caller should own.
pub fn map_row_chunks<T, R, F>(data: &[T], row_len: usize, granularity: usize, f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(usize, &[T]) -> R + Sync,
{
    if row_len == 0 || data.is_empty() {
        return Vec::new();
    }
    let rows = data.len() / row_len;
    let per = rows_per_chunk(rows, granularity);
    let chunk_len = per * row_len;
    if chunk_len == 0 || chunk_len >= data.len() {
        return vec![f(0, data)];
    }
    let chunks: Vec<(usize, &[T])> = data
        .chunks(chunk_len)
        .enumerate()
        .map(|(i, c)| (i * per, c))
        .collect();
    let mut out: Vec<Option<R>> = (0..chunks.len()).map(|_| None).collect();
    std::thread::scope(|s| {
        for ((start, chunk), slot) in chunks.into_iter().zip(out.iter_mut()) {
            let f = &f;
            s.spawn(move || *slot = Some(f(start, chunk)));
        }
    });
    out.into_iter().map(|o| o.expect("every chunk ran")).collect()
}

/// Map each chunk of a *virtual* row range to a value, in parallel, returning
/// the per-chunk results in order for the caller to combine.
///
/// The reduction counterpart to [`map_row_chunks`] for a pass with no backing
/// slice to walk — a fold over samples computed on the fly rather than stored.
/// That distinction is the point: it lets a two-pass algorithm recompute its
/// kernel in the first pass instead of parking a full-resolution intermediate
/// in memory between the passes.
///
/// `f` receives `(start_row, row_count)` rather than a slice, since there is
/// nothing to borrow; callers reconstruct `(x, y)` exactly as they do for the
/// slice-backed helpers.
pub fn map_row_ranges<R, F>(rows: usize, granularity: usize, f: F) -> Vec<R>
where
    R: Send,
    F: Fn(usize, usize) -> R + Sync,
{
    if rows == 0 {
        return Vec::new();
    }
    let per = rows_per_chunk(rows, granularity);
    if per == 0 || per >= rows {
        return vec![f(0, rows)];
    }
    let ranges: Vec<(usize, usize)> = (0..rows)
        .step_by(per)
        .map(|start| (start, per.min(rows - start)))
        .collect();
    let mut out: Vec<Option<R>> = (0..ranges.len()).map(|_| None).collect();
    std::thread::scope(|s| {
        for ((start, n), slot) in ranges.into_iter().zip(out.iter_mut()) {
            let f = &f;
            s.spawn(move || *slot = Some(f(start, n)));
        }
    });
    out.into_iter().map(|o| o.expect("every chunk ran")).collect()
}

/// Split an *output* row range across workers, giving each `f(row)` for the
/// rows it owns. Used where the output is smaller than the input (the
/// subsampled gain plane) and the natural decomposition is over destination
/// rows, which also guarantees no two workers touch the same output element.
pub fn for_each_out_row_chunk_mut<T, F>(out: &mut [T], row_len: usize, f: F)
where
    T: Send,
    F: Fn(usize, &mut [T]) + Sync,
{
    for_each_row_chunk_mut(out, row_len, 1, f);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutating_chunks_covers_every_element_exactly_once() {
        for len in [0usize, 1, 7, 64, 1000, 4096] {
            let row_len = 8;
            let n = len * row_len;
            let mut v = vec![0u32; n];
            for_each_row_chunk_mut(&mut v, row_len, 1, |start, chunk| {
                for (i, x) in chunk.iter_mut().enumerate() {
                    *x = (start * row_len + i) as u32;
                }
            });
            for (i, x) in v.iter().enumerate() {
                assert_eq!(*x, i as u32, "len={len} index {i}");
            }
        }
    }

    #[test]
    fn start_row_is_correct_for_every_chunk() {
        let row_len = 5;
        let rows = 97;
        let mut v = vec![0usize; rows * row_len];
        for_each_row_chunk_mut(&mut v, row_len, 1, |start, chunk| {
            for (r, row) in chunk.chunks_mut(row_len).enumerate() {
                for cell in row {
                    *cell = start + r;
                }
            }
        });
        for r in 0..rows {
            for c in 0..row_len {
                assert_eq!(v[r * row_len + c], r);
            }
        }
    }

    #[test]
    fn granularity_keeps_chunk_starts_aligned() {
        let row_len = 4;
        let rows = 101;
        let v = vec![0u8; rows * row_len];
        let starts = map_row_chunks(&v, row_len, 2, |start, _| start);
        for s in &starts {
            assert_eq!(s % 2, 0, "chunk start {s} not a multiple of 2");
        }
        assert_eq!(starts.first(), Some(&0));
    }

    #[test]
    fn map_chunks_reduces_correctly() {
        let row_len = 3;
        let v: Vec<u64> = (0..(300 * row_len as u64)).collect();
        let partials = map_row_chunks(&v, row_len, 1, |_, c| c.iter().sum::<u64>());
        let got: u64 = partials.iter().sum();
        assert_eq!(got, v.iter().sum::<u64>());
    }

    #[test]
    fn single_row_and_empty_inputs_are_safe() {
        let mut empty: Vec<u8> = Vec::new();
        for_each_row_chunk_mut(&mut empty, 4, 1, |_, _| panic!("must not run"));
        assert!(map_row_chunks(&empty, 4, 1, |_, _| 1u8).is_empty());

        let mut one = vec![0u8; 4];
        for_each_row_chunk_mut(&mut one, 4, 1, |s, c| {
            assert_eq!(s, 0);
            c[0] = 9;
        });
        assert_eq!(one[0], 9);
    }
}
