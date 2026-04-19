use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{self, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

use super::{format_elapsed_trace, is_ci, is_pretty, should_colorize};

/// Actions shorter than this emit only the end log. Longer actions also
/// emit a deferred "Doing X…" start log so in-between traces have context.
const DEFERRED_START_THRESHOLD: Duration = Duration::from_secs(2);

/// Start a timed span. Emits a deferred start log at DEBUG after
/// `DEFERRED_START_THRESHOLD` (only if the work is still running) and an
/// end log at TRACE on drop. In pretty mode both calls are no-ops because
/// no tracing subscriber is installed.
pub fn timed(label: &str) -> TimedSpan {
    TimedSpan::new(label)
}

pub struct TimedSpan {
    label: String,
    start: Instant,
    cancel: Arc<(Mutex<bool>, Condvar)>,
}

impl TimedSpan {
    fn new(label: &str) -> Self {
        let cancel = Arc::new((Mutex::new(false), Condvar::new()));

        if !is_pretty() {
            let cancel_clone = cancel.clone();
            let label_clone = label.to_string();
            thread::spawn(move || {
                let (lock, cvar) = &*cancel_clone;
                let cancelled = lock.lock().unwrap();
                let (cancelled, _) = cvar
                    .wait_timeout(cancelled, DEFERRED_START_THRESHOLD)
                    .unwrap();
                if !*cancelled {
                    tracing::debug!("{label_clone}…");
                }
            });
        }

        Self {
            label: label.to_string(),
            start: Instant::now(),
            cancel,
        }
    }
}

impl Drop for TimedSpan {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.cancel;
        *lock.lock().unwrap() = true;
        cvar.notify_all();
        let time = format_elapsed_trace(self.start.elapsed());
        tracing::trace!("{} {time}", self.label);
    }
}

// ── Tracing scope support ───────────────────────────────────────────────────

/// Custom timer that formats as local HH:MM:SS.mmm.
pub struct LocalTimer;

impl tracing_subscriber::fmt::time::FormatTime for LocalTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        #[cfg(unix)]
        {
            let dur = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let epoch = dur.as_secs() as libc::time_t;
            let millis = dur.subsec_millis();
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            unsafe { libc::localtime_r(&epoch, &mut tm) };
            write!(
                w,
                "{:02}:{:02}:{:02}.{:03}",
                tm.tm_hour, tm.tm_min, tm.tm_sec, millis
            )
        }
        #[cfg(not(unix))]
        {
            let dur = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let total_secs = dur.as_secs();
            let millis = dur.subsec_millis();
            let hours = (total_secs % 86400) / 3600;
            let minutes = (total_secs % 3600) / 60;
            let seconds = total_secs % 60;
            write!(
                w,
                "{:02}:{:02}:{:02}.{:03}",
                hours, minutes, seconds, millis
            )
        }
    }
}

/// Data stored in span extensions to carry the scope label.
struct SpanScope(String);

/// Visitor that extracts the `scope` field from span attributes.
struct ScopeVisitor(Option<String>);

impl tracing::field::Visit for ScopeVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "scope" {
            self.0 = Some(value.to_string());
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "scope" {
            self.0 = Some(format!("{value:?}").trim_matches('"').to_string());
        }
    }
}

/// Layer that captures `scope` fields from spans into extensions.
pub struct ScopeLayer;

impl<S> tracing_subscriber::Layer<S> for ScopeLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = ScopeVisitor(None);
        attrs.record(&mut visitor);
        if let Some(scope) = visitor.0
            && let Some(span) = ctx.span(id)
        {
            span.extensions_mut().insert(SpanScope(scope));
        }
    }
}

/// Custom event format: `HH:MM:SS.mmm LEVEL [scope] message`
/// In CI mode: no ANSI colors.
pub struct ScopeFormat;

impl<S, N> FormatEvent<S, N> for ScopeFormat
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        use tracing_subscriber::fmt::time::FormatTime;

        if !is_ci() {
            if should_colorize() {
                write!(writer, "\x1b[2m")?;
                LocalTimer.format_time(&mut writer)?;
                write!(writer, "\x1b[22m")?;
            } else {
                LocalTimer.format_time(&mut writer)?;
            }
        }

        // Level (right-aligned, 5 chars)
        let level = *event.metadata().level();
        if should_colorize() {
            let color = match level {
                tracing::Level::ERROR => "\x1b[31m",
                tracing::Level::WARN => "\x1b[33m",
                tracing::Level::INFO => "\x1b[32m",
                tracing::Level::DEBUG => "\x1b[34m",
                tracing::Level::TRACE => "\x1b[35m",
            };
            if is_ci() {
                write!(writer, "{color}{level:>5}\x1b[0m ")?;
            } else {
                write!(writer, " {color}{level:>5}\x1b[0m ")?;
            }
        } else if is_ci() {
            write!(writer, "{level:>5} ")?;
        } else {
            write!(writer, " {level:>5} ")?;
        }

        // Scope from innermost span (leaf -> root, take first match)
        if let Some(scope) = ctx.event_scope() {
            for span in scope {
                if let Some(data) = span.extensions().get::<SpanScope>() {
                    write!(writer, "[{}] ", data.0)?;
                    break;
                }
            }
        }

        // Message
        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// Create a tracing span that provides `[name]` scope prefix in verbose output.
pub fn scope(name: &str) -> tracing::Span {
    tracing::info_span!("", scope = %name)
}
