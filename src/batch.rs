use std::num::NonZeroUsize;

use anyhow::{Context, Result};

/// Transaction boundaries count complete logical track changes. Call queued()
/// only after all parts of a change (e.g. artwork remove + add) are queued.
/// Playlist changes are deliberately outside these track batches.
pub(crate) struct TrackBatches {
    limit: usize,
    total_tracks: usize,
    pending: usize,
    committed: usize,
    completed_batches: usize,
    phase: &'static str,
}

impl TrackBatches {
    pub(crate) fn new(
        limit: Option<NonZeroUsize>,
        total_tracks: usize,
        phase: &'static str,
    ) -> Self {
        Self {
            limit: limit.map_or(total_tracks.max(1), NonZeroUsize::get),
            total_tracks,
            pending: 0,
            committed: 0,
            completed_batches: 0,
            phase,
        }
    }

    pub(crate) fn queued(&mut self, commit: impl FnOnce() -> Result<()>) -> Result<()> {
        self.pending += 1;
        if self.pending == self.limit {
            self.finish(commit)?;
        }
        Ok(())
    }

    /// Commits the final partial batch, but never an empty transaction.
    pub(crate) fn finish(&mut self, commit: impl FnOnce() -> Result<()>) -> Result<()> {
        if self.pending == 0 {
            return Ok(());
        }
        let batch = self.completed_batches + 1;
        let total_batches = self.total_tracks.div_ceil(self.limit);
        println!(
            "Committing {} batch {batch}/{total_batches}: {} track change(s) ({} / {} already committed)…",
            self.phase, self.pending, self.committed, self.total_tracks,
        );
        commit().with_context(|| format!(
            "{} batch {batch}/{total_batches} failed; {} earlier batch(es), {} track change(s), completed. Recover if prompted and rerun the same sync to finish",
            self.phase, self.completed_batches, self.committed,
        ))?;
        self.committed += self.pending;
        self.pending = 0;
        self.completed_batches = batch;
        println!(
            "Committed {} batch {batch}/{total_batches}: {} / {} track change(s) complete.",
            self.phase, self.committed, self.total_tracks,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries_cover_default_partial_exact_large_and_empty_batches() {
        for (limit, count, expected) in [
            (None, 5, vec![5]),
            (Some(2), 5, vec![2, 2, 1]),
            (Some(2), 4, vec![2, 2]),
            (Some(1), 3, vec![1, 1, 1]),
            (Some(20), 5, vec![5]),
            (Some(usize::MAX), 5, vec![5]),
            (Some(2), 0, vec![]),
            (None, 0, vec![]),
        ] {
            let mut batches = TrackBatches::new(limit.and_then(NonZeroUsize::new), count, "test");
            let mut queued = Vec::new();
            let mut actual = Vec::new();
            for index in 0..count {
                queued.push(index);
                batches
                    .queued(|| {
                        actual.push(queued.len());
                        queued.clear();
                        Ok(())
                    })
                    .unwrap();
            }
            batches
                .finish(|| {
                    actual.push(queued.len());
                    queued.clear();
                    Ok(())
                })
                .unwrap();
            assert_eq!(actual, expected);
            assert_eq!(batches.committed, count);
            assert!(queued.is_empty());
        }
    }

    #[test]
    fn artwork_replacement_is_one_complete_change_not_two_batches() {
        let mut batches = TrackBatches::new(NonZeroUsize::new(1), 2, "copy/artwork");
        let mut changes = Vec::new();
        for _ in 0..2 {
            changes.push("remove old record");
            changes.push("add record with artwork");
            batches
                .queued(|| {
                    assert_eq!(changes, ["remove old record", "add record with artwork"]);
                    changes.clear();
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(batches.completed_batches, 2);
        batches
            .finish(|| panic!("must not commit an empty tail"))
            .unwrap();
    }

    #[test]
    fn failed_batch_does_not_advance_completed_counts() {
        let mut batches = TrackBatches::new(NonZeroUsize::new(2), 5, "copy/artwork");
        batches.queued(|| panic!("not full yet")).unwrap();
        batches.queued(|| Ok(())).unwrap();
        batches.queued(|| panic!("not full yet")).unwrap();
        let error = batches
            .queued(|| anyhow::bail!("simulated interruption"))
            .unwrap_err();
        assert_eq!(batches.committed, 2);
        assert_eq!(batches.completed_batches, 1);
        assert_eq!(batches.pending, 2);
        assert!(error
            .to_string()
            .contains("1 earlier batch(es), 2 track change(s)"));
    }
}
