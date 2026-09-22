//! Console logging that also feeds the admin panel.
//!
//! Every log line goes to stdout (coloured, for people) and into a ring
//! buffer plus a broadcast channel (for the panel's live console).

use garnet_admin::api::LogLine;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

const HISTORY: usize = 500;

#[derive(Clone)]
pub struct LogSink {
    history: Arc<Mutex<VecDeque<LogLine>>>,
    pub sender: broadcast::Sender<LogLine>,
}

impl LogSink {
    pub fn history(&self) -> Vec<LogLine> {
        self.history.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
    }

    fn push(&self, line: LogLine) {
        let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        if history.len() >= HISTORY {
            history.pop_front();
        }
        history.push_back(line.clone());
        let _ = self.sender.send(line);
    }
}

struct PanelLayer {
    sink: LogSink,
}

struct MessageVisitor(String);

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        }
    }
}

impl<S: Subscriber> Layer<S> for PanelLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        let meta = event.metadata();
        self.sink.push(LogLine {
            time: chrono::Local::now().format("%H:%M:%S").to_string(),
            level: meta.level().to_string(),
            target: meta.target().to_owned(),
            message: visitor.0,
        });
    }
}

/// Installs the global logger. `RUST_LOG` overrides the default filter.
pub fn init() -> LogSink {
    let (sender, _) = broadcast::channel(256);
    let sink = LogSink {
        history: Arc::new(Mutex::new(VecDeque::with_capacity(HISTORY))),
        sender,
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,garnet=info,hyper=warn,reqwest=warn,wasmtime=warn,cranelift=warn,h2=warn,rustls=warn")
    });
    let stdout = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_timer(tracing_subscriber::fmt::time::ChronoLocal::new("%H:%M:%S".into()))
        .with_filter(filter.clone());
    let panel = PanelLayer { sink: sink.clone() }.with_filter(filter);
    tracing_subscriber::registry().with(stdout).with(panel).init();
    sink
}
