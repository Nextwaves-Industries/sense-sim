//! What a request has spent, so a long or runaway task stops and asks instead of quietly running
//! up a bill. Every model call goes through [`Metered`], the reviewer's and summaries included.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use crossbeam_channel::Sender;

use super::Event;
use super::llm::{Live, Message, Provider, ToolDef};

/// When to stop and ask. Zero means no limit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    /// Tokens the provider charges in full: input not read from its cache, and output.
    pub tokens: u64,
    pub calls: u64,
    pub minutes: u64,
}

pub struct Budget {
    tokens: AtomicU64,
    calls: AtomicU64,
    started: Instant,
    limits: Mutex<Limits>,
}

impl Default for Budget {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl Budget {
    pub fn new(limits: Limits) -> Self {
        Self { tokens: AtomicU64::new(0), calls: AtomicU64::new(0), started: Instant::now(), limits: Mutex::new(limits) }
    }

    /// Why this should stop now, in words for the user.
    pub fn exceeded(&self) -> Option<String> {
        let limits = *self.limits.lock().ok()?;
        let tokens = self.tokens.load(Ordering::Relaxed);
        let calls = self.calls.load(Ordering::Relaxed);
        let minutes = self.started.elapsed().as_secs() / 60;
        if limits.tokens > 0 && tokens >= limits.tokens {
            return Some(format!("This request has used {} tokens, past its budget of {}.", thousands(tokens), thousands(limits.tokens)));
        }
        if limits.calls > 0 && calls >= limits.calls {
            return Some(format!("This request has made {calls} model calls, past its budget of {}.", limits.calls));
        }
        if limits.minutes > 0 && minutes >= limits.minutes {
            return Some(format!("This request has run for {minutes} minutes, past its budget of {}.", limits.minutes));
        }
        None
    }

    /// The user said to keep going: allow as much again, so it asks again rather than never.
    pub fn extend(&self) {
        if let Ok(mut l) = self.limits.lock() {
            let again = |limit: u64, used: u64| if limit > 0 { used.max(limit) + limit } else { 0 };
            *l = Limits {
                tokens: again(l.tokens, self.tokens.load(Ordering::Relaxed)),
                calls: again(l.calls, self.calls.load(Ordering::Relaxed)),
                minutes: again(l.minutes, self.started.elapsed().as_secs() / 60),
            };
        }
    }
}

pub fn thousands(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.2}M", n as f64 / 1e6),
    }
}

/// A provider that writes down what it spends.
pub(super) struct Metered {
    pub inner: Arc<dyn Provider>,
    pub budget: Arc<Budget>,
    pub ev: Sender<Event>,
}

impl Metered {
    fn write_down(&self, reply: &Result<Message>) {
        // A failed call can still be charged, and still counts towards a runaway.
        self.budget.calls.fetch_add(1, Ordering::Relaxed);
        if let Ok(Message { usage: Some(u), .. }) = reply {
            self.budget.tokens.fetch_add(u.billable(), Ordering::Relaxed);
            let _ = self.ev.send(Event::Usage { model: self.inner.model(), usage: *u });
        }
    }
}

impl Provider for Metered {
    fn complete(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> Result<Message> {
        let reply = self.inner.complete(system, messages, tools);
        self.write_down(&reply);
        reply
    }

    fn complete_live(&self, system: &str, messages: &[Message], tools: &[ToolDef], live: &mut dyn FnMut(Live) -> bool) -> Result<Message> {
        let reply = self.inner.complete_live(system, messages, tools, live);
        self.write_down(&reply);
        reply
    }

    fn model(&self) -> String {
        self.inner.model()
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::unbounded;

    use super::super::llm::{Block, Role, Usage};
    use super::*;

    struct Spends(Usage);
    impl Provider for Spends {
        fn complete(&self, _: &str, _: &[Message], _: &[ToolDef]) -> Result<Message> {
            Ok(Message { role: Role::Assistant, content: vec![Block::Text("ok".into())], raw: None, usage: Some(self.0) })
        }
        fn model(&self) -> String {
            "test-model".into()
        }
    }

    #[test]
    fn no_limit_never_stops() {
        let b = Budget::default();
        b.tokens.store(u64::MAX / 2, Ordering::Relaxed);
        b.calls.store(u64::MAX / 2, Ordering::Relaxed);
        assert!(b.exceeded().is_none());
    }

    #[test]
    fn each_limit_stops_and_says_which() {
        let b = Budget::new(Limits { tokens: 100, ..Default::default() });
        assert!(b.exceeded().is_none());
        b.tokens.store(100, Ordering::Relaxed);
        assert!(b.exceeded().unwrap().contains("tokens"));

        let b = Budget::new(Limits { calls: 2, ..Default::default() });
        b.calls.store(2, Ordering::Relaxed);
        assert!(b.exceeded().unwrap().contains("model calls"));
    }

    #[test]
    fn keeping_going_allows_as_much_again_then_asks_again() {
        let b = Budget::new(Limits { tokens: 100, ..Default::default() });
        b.tokens.store(150, Ordering::Relaxed);
        assert!(b.exceeded().is_some());
        b.extend();
        assert!(b.exceeded().is_none(), "it has room again");
        b.tokens.store(249, Ordering::Relaxed);
        assert!(b.exceeded().is_none());
        b.tokens.store(250, Ordering::Relaxed);
        assert!(b.exceeded().is_some(), "and it asks again");
    }

    #[test]
    fn every_call_is_counted_and_cached_input_isnt_charged_in_full() {
        let budget = Arc::new(Budget::new(Limits { tokens: 1000, ..Default::default() }));
        let (ev, events) = unbounded();
        let usage = Usage { input: 800, cached: 700, output: 50 };
        let p = Metered { inner: Arc::new(Spends(usage)), budget: budget.clone(), ev };
        p.complete("s", &[], &[]).unwrap();
        assert_eq!(budget.tokens.load(Ordering::Relaxed), 150);
        assert_eq!(budget.calls.load(Ordering::Relaxed), 1);
        assert!(matches!(events.try_recv().unwrap(), Event::Usage { model, .. } if model == "test-model"));
    }

    #[test]
    fn numbers_read_well() {
        assert_eq!(thousands(950), "950");
        assert_eq!(thousands(12_300), "12.3k");
        assert_eq!(thousands(2_000_000), "2.00M");
    }
}
