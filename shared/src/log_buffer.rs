use chrono::{DateTime, Local};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::{
    fmt::{format::Writer, FormatEvent, FormatFields},
    registry::LookupSpan,
};

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: DateTime<Local>,
    pub level: Level,
    pub target: String,
    pub message: String,
}

impl LogEntry {
    pub fn level_color(&self) -> (u8, u8, u8) {
        match self.level {
            Level::ERROR => (255, 0, 0),     // Red
            Level::WARN => (255, 165, 0),    // Orange
            Level::INFO => (0, 255, 0),      // Green
            Level::DEBUG => (173, 216, 230), // Light Blue
            Level::TRACE => (128, 128, 128), // Gray
        }
    }
    
    pub fn level_symbol(&self) -> &'static str {
        match self.level {
            Level::ERROR => "❌",
            Level::WARN => "⚠️ ",
            Level::INFO => "ℹ️ ",
            Level::DEBUG => "🐛",
            Level::TRACE => "🔍",
        }
    }
}

#[derive(Debug)]
pub struct LogBuffer {
    entries: Arc<Mutex<VecDeque<LogEntry>>>,
    max_size: usize,
}

impl LogBuffer {
    pub fn new(max_size: usize) -> Self {
        Self {
            entries: Arc::new(Mutex::new(VecDeque::with_capacity(max_size))),
            max_size,
        }
    }

    pub fn add_entry(&self, entry: LogEntry) {
        let mut entries = self.entries.lock();
        if entries.len() >= self.max_size {
            entries.pop_front();
        }
        entries.push_back(entry);
    }

    pub fn get_entries(&self) -> Vec<LogEntry> {
        self.entries.lock().iter().cloned().collect()
    }

    pub fn get_latest(&self, count: usize) -> Vec<LogEntry> {
        let entries = self.entries.lock();
        entries
            .iter()
            .rev()
            .take(count)
            .rev()
            .cloned()
            .collect()
    }

    pub fn clear(&self) {
        self.entries.lock().clear();
    }

    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }
}

impl Clone for LogBuffer {
    fn clone(&self) -> Self {
        Self {
            entries: Arc::clone(&self.entries),
            max_size: self.max_size,
        }
    }
}

pub struct TuiLogFormatter {
    buffer: LogBuffer,
    suppress_writer_output: bool,
}

impl TuiLogFormatter {
    pub fn new(buffer: LogBuffer) -> Self {
        Self { 
            buffer,
            suppress_writer_output: false,
        }
    }

    pub fn new_with_suppression(buffer: LogBuffer, suppress_writer_output: bool) -> Self {
        Self { 
            buffer,
            suppress_writer_output,
        }
    }
}

impl<S, N> FormatEvent<S, N> for TuiLogFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let metadata = event.metadata();
        let level = *metadata.level();
        let target = metadata.target().to_string();

        // Extract the message from the event
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let message = visitor.message;

        // Add to buffer
        let entry = LogEntry {
            timestamp: Local::now(),
            level,
            target,
            message: message.clone(),
        };
        self.buffer.add_entry(entry);

        // Only write to the writer if not suppressed (for non-TUI mode or debugging)
        if !self.suppress_writer_output {
            writeln!(writer, "{}", message)
        } else {
            // In TUI mode, completely suppress output to prevent interference
            Ok(())
        }
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
            // Remove quotes from the debug format
            if self.message.starts_with('"') && self.message.ends_with('"') {
                self.message = self.message[1..self.message.len()-1].to_string();
            }
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }
}