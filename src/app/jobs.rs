//! Pulling records in off the internet.

use crate::{pull, Defalt};

impl Defalt {
    /// Start a pull, and rescan when one finishes.
    pub fn begin_pull(&mut self) {
        let query = self.pull_query.trim().to_string();
        if query.is_empty() {
            return;
        }
        match pull::start(&self.root, &query, self.pull_duration_ms) {
            Ok(job) => {
                self.pulls.insert(0, job);
                self.pull_query.clear();
                self.pull_duration_ms = None;
                self.catalogue.clear();
            }
            Err(error) => self.say(&error),
        }
    }

    /// Take a suggestion: its exact name goes in the box, and its length goes
    /// to the resolver.
    pub fn take_suggestion(&mut self, at: usize) {
        let Some(found) = self.catalogue.showing.get(at).cloned() else { return };
        self.pull_query = found.query();
        self.pull_duration_ms = Some(found.duration_ms).filter(|ms| *ms > 0);
        self.catalogue.clear();
    }

    pub(crate) fn poll_pulls(&mut self) {
        let mut arrived = false;
        for job in self.pulls.iter_mut() {
            let was_over = job.stage.is_over();
            job.poll();
            if !was_over && matches!(job.stage, pull::Stage::Done { .. }) {
                arrived = true;
            }
        }
        if arrived {
            // The importer wrote a row; the crate has to be told.
            self.reload_library();
            self.say("Pulled in. It is in your music folder.");
        }
        // Finished jobs are worth keeping on screen for a moment, not
        // forever: the crate is where a record lives once it has arrived.
        self.pulls.retain(|job| {
            !job.stage.is_over() || job.started.elapsed().as_secs() < 25
        });
    }

    pub fn can_pull(&self) -> bool {
        pull::python(&self.root).is_some()
    }
}
