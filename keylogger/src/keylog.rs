use chrono::{DateTime, TimeZone, Utc};
use rdev::{Event, EventType, Key};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncWriteExt, BufWriter},
    select,
    sync::mpsc as tokio_mpsc, // Tokio channel for cross-thread reporting
};

#[derive(Serialize, Debug)]
pub struct KeylogEntry {
    pub timestamp: String,
    pub keys: HashMap<String, KeyStats>,
}

#[derive(Serialize, Default, Clone, Debug)]
pub struct KeyStats {
    #[serde(skip_serializing_if = "is_zero")]
    pub shift: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub raw: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub bare: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub ctrl: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub alt: u32,
    #[serde(rename = "ctrl+shift", skip_serializing_if = "is_zero")]
    pub ctrl_shift: u32,
    #[serde(rename = "ctrl+alt", skip_serializing_if = "is_zero")]
    pub ctrl_alt: u32,
    #[serde(rename = "shift+alt", skip_serializing_if = "is_zero")]
    pub shift_alt: u32,
}

impl KeyStats {
    pub fn increment(&mut self, modifiers: &ModifiersState) {
        self.raw += 1;
        match (modifiers.shift, modifiers.ctrl, modifiers.alt) {
            (false, false, false) => self.bare += 1,
            (true, false, false) => self.shift += 1,
            (false, true, false) => self.ctrl += 1,
            (false, false, true) => self.alt += 1,
            (true, true, false) => self.ctrl_shift += 1,
            (false, true, true) => self.ctrl_alt += 1,
            (true, false, true) => self.shift_alt += 1,
            _ => {}
        }
    }
}

struct ModifiersState {
    shift: bool,
    ctrl: bool,
    alt: bool,
}

impl ModifiersState {
    fn new() -> Self {
        Self {
            shift: false,
            ctrl: false,
            alt: false,
        }
    }

    fn update(&mut self, key: &Key, pressed: bool) {
        match key {
            Key::ShiftLeft | Key::ShiftRight => self.shift = pressed,
            Key::ControlLeft | Key::ControlRight => self.ctrl = pressed,
            Key::Alt | Key::AltGr => self.alt = pressed,
            _ => {}
        }
    }

    fn is_modifier(key: &Key) -> bool {
        matches!(key, Key::ShiftLeft | Key::ShiftRight | Key::ControlLeft | Key::ControlRight | Key::Alt | Key::AltGr)
    }
}

fn is_zero(val: &u32) -> bool {
    *val == 0
}

fn format_key(key: &Key) -> String {
    let s = format!("{:?}", key).to_lowercase();
    if s.starts_with("key") && s.len() > 3 {
        s[3..].to_string()
    } else {
        s
    }
}

fn unix_now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("SystemTime::duration_since(UNIX_EPOCH) failed: Time went backwards.")
}

fn duration_to_datetime(duration: Duration) -> DateTime<Utc> {
    Utc.timestamp_opt(duration.as_secs_f64() as i64, 
        duration.subsec_nanos()).single().unwrap_or_else(|| Utc::now())
}

pub struct KeyAggregator {
    interval_length: Duration,
    interval_start: SystemTime,
    buffer: HashMap<String, KeyStats>,
    modifiers: ModifiersState,
}

impl KeyAggregator {
    pub fn new(interval_length: Duration) -> Self {
        let now = unix_now();
        Self {
            interval_length,
            interval_start: SystemTime::now(),
            buffer: HashMap::new(),
            modifiers: ModifiersState::new(),
        }
    }

    fn ingest(&mut self, event: Event) {
        match event.event_type {
            EventType::KeyPress(key) => {
                self.modifiers.update(&key, true);
                if !ModifiersState::is_modifier(&key) {
                    let name = format_key(&key);
                    self.buffer
                        .entry(name)
                        .or_default()
                        .increment(&self.modifiers);
                }
            }
            EventType::KeyRelease(key) => {
                self.modifiers.update(&key, false);
            }
            _ => {}
        }
    }

    fn flush(&mut self) -> Option<KeylogEntry> {
        if self.buffer.is_empty() {
            self.align_time();
            return None;
        }

        let timestamp = self.interval_start
            .duration_since(UNIX_EPOCH)
            .map(|d| Utc.timestamp_opt(d.as_secs() as i64, d.subsec_nanos()).unwrap())
            .unwrap_or_else(|_| Utc::now())
            .to_rfc3339();

        let entry = KeylogEntry {
            timestamp,
            keys: std::mem::take(&mut self.buffer),
        };

        self.align_time();
        Some(entry)
    }

    fn align_time(&mut self) {
        self.interval_start += self.interval_length;
    }
}

pub struct KeyLoggerHandle {
    pub event_tx: mpsc::Sender<Event>, 
    pub report_rx: tokio_mpsc::Receiver<KeylogEntry>, 
    pub thread_handle: thread::JoinHandle<()>,
}

pub fn spawn_keylogger(interval: Duration) -> KeyLoggerHandle {
    let (event_tx, event_rx) = mpsc::channel::<Event>();
    let (report_tx, report_rx) = tokio_mpsc::channel::<KeylogEntry>(32);

    let handle = thread::spawn(move || {
        let mut aggregator = KeyAggregator::new(interval);

        let now = SystemTime::now();
        if let Ok(dur) = now.duration_since(UNIX_EPOCH) {
            let remainder = dur.as_nanos() % interval.as_nanos();
            aggregator.interval_start = now - Duration::from_nanos(remainder as u64);
        }

        loop {
            let now = SystemTime::now();
            let target_time = aggregator.interval_start + interval;
            let time_remaining = target_time.duration_since(now).unwrap_or(Duration::ZERO);

            match event_rx.recv_timeout(time_remaining) {
                Ok(event) => {
                    aggregator.ingest(event);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(report) = aggregator.flush() {
                        if report_tx.blocking_send(report).is_err() {
                            break;
                        }
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }
    });

    KeyLoggerHandle { event_tx, report_rx, thread_handle: handle }
}