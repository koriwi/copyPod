use std::{collections::HashMap, io::Write, time::Instant};

use libopod::ProgressEvent;

/// Line-based output also works when redirected, without terminal escape
/// sequences. Every event is flushed before libopod starts the reported work.
pub(crate) struct ProgressReporter {
    started: Instant,
    labels: HashMap<String, String>,
}

impl ProgressReporter {
    pub(crate) fn new(labels: HashMap<String, String>) -> Self {
        Self {
            started: Instant::now(),
            labels,
        }
    }

    pub(crate) fn add_media(&mut self, path: &str, label: String) {
        self.labels.insert(path.to_owned(), label);
    }

    pub(crate) fn report(&self, event: ProgressEvent<'_>) {
        self.write_event(event, &mut std::io::stdout().lock());
    }

    fn write_event(&self, event: ProgressEvent<'_>, output: &mut impl Write) {
        let message = match event {
            ProgressEvent::Phase(name) => format!("{name}…"),
            ProgressEvent::Item {
                operation,
                current,
                total,
                name,
            } => {
                let label = self.labels.get(name).map_or(name, String::as_str);
                format!("{operation} [{current}/{total}]: {label}")
            }
            _ => return,
        };
        // Tags can contain newlines or terminal controls. Keep each event on
        // one line, including when stdout is a log file or pipe.
        let message: String = message
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let seconds = self.started.elapsed().as_secs();
        // Logging must never panic or interrupt a recoverable device write
        // just because a pipe closed or the terminal disappeared.
        let _ = writeln!(
            output,
            "  [{:02}:{:02}] {message}",
            seconds / 60,
            seconds % 60
        );
        let _ = output.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Output {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn reports_real_work_with_counters_song_names_and_immediate_flush() {
        let mut reporter = ProgressReporter::new(HashMap::new());
        reporter.add_media(
            "iPod_Control/Music/F00/ABCD.mp3",
            "Artist — Song\nTitle".to_owned(),
        );
        let mut output = Output::default();
        reporter.write_event(ProgressEvent::Phase("Backing up"), &mut output);
        reporter.write_event(
            ProgressEvent::Item {
                operation: "Installing",
                current: 2,
                total: 3,
                name: "iPod_Control/Music/F00/ABCD.mp3",
            },
            &mut output,
        );
        reporter.write_event(
            ProgressEvent::Item {
                operation: "Installing",
                current: 3,
                total: 3,
                name: "iPod_Control/iTunes/iTunesDB",
            },
            &mut output,
        );
        let text = String::from_utf8(output.bytes).unwrap();
        assert!(text.contains("Backing up…"));
        assert!(text.contains("Installing [2/3]: Artist — Song Title"));
        assert!(text.contains("Installing [3/3]: iPod_Control/iTunes/iTunesDB"));
        assert_eq!(text.lines().count(), 3);
        assert_eq!(output.flushes, 3);
    }

    #[test]
    fn recovery_reports_paths_without_needing_library_labels() {
        let reporter = ProgressReporter::new(HashMap::new());
        let mut output = Output::default();
        reporter.write_event(
            ProgressEvent::Phase("Reading recovery journal"),
            &mut output,
        );
        reporter.write_event(
            ProgressEvent::Item {
                operation: "Restoring recovery backup",
                current: 1,
                total: 2,
                name: "iPod_Control/iTunes/iTunesDB",
            },
            &mut output,
        );
        reporter.write_event(
            ProgressEvent::Phase("Removing recovery journal and backups"),
            &mut output,
        );
        let text = String::from_utf8(output.bytes).unwrap();
        assert!(text.contains("Reading recovery journal"));
        assert!(text.contains("Restoring recovery backup [1/2]: iPod_Control/iTunes/iTunesDB"));
        assert!(text.contains("Removing recovery journal and backups"));
        assert_eq!(output.flushes, 3);
    }

    #[test]
    fn rename_preservation_publication_and_terminal_recovery_are_visible() {
        let reporter = ProgressReporter::new(HashMap::new());
        let mut output = Output::default();
        let phases = [
            "Rechecking device before installation",
            "Preserving original file by rename",
            "Publishing verified replacement",
            "Transaction already rolled back; keeping restored files",
        ];
        for phase in phases {
            reporter.write_event(ProgressEvent::Phase(phase), &mut output);
        }
        let text = String::from_utf8(output.bytes).unwrap();
        for phase in phases {
            assert!(text.contains(phase));
        }
        assert_eq!(output.flushes, phases.len());
        assert_eq!(text.lines().count(), phases.len());
    }

    #[test]
    fn incremental_artwork_and_truncation_progress_are_flushed() {
        let reporter = ProgressReporter::new(HashMap::new());
        let mut output = Output::default();
        let path = "iPod_Control/Artwork/F1060_1.ithmb";
        reporter.write_event(
            ProgressEvent::Item {
                operation: "Appending artwork",
                current: 3,
                total: 7,
                name: path,
            },
            &mut output,
        );
        for phase in [
            "Reserving recovery journal space",
            "Preparing verified thumbnail suffix",
            "Appending verified thumbnail suffix",
        ] {
            reporter.write_event(ProgressEvent::Phase(phase), &mut output);
        }
        reporter.write_event(
            ProgressEvent::Item {
                operation: "Truncating appended thumbnail",
                current: 5,
                total: 6,
                name: path,
            },
            &mut output,
        );
        let text = String::from_utf8(output.bytes).unwrap();
        assert!(text.contains(&format!("Appending artwork [3/7]: {path}")));
        assert!(text.contains("Preparing verified thumbnail suffix"));
        assert!(text.contains("Appending verified thumbnail suffix"));
        assert!(text.contains(&format!("Truncating appended thumbnail [5/6]: {path}")));
        assert_eq!(output.flushes, 5);
        assert_eq!(text.lines().count(), 5);
    }

    #[test]
    fn broken_progress_output_does_not_abort_a_transaction() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
        }
        ProgressReporter::new(HashMap::new())
            .write_event(ProgressEvent::Phase("Installing"), &mut Broken);
    }
}
