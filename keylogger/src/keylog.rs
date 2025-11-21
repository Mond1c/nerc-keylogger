use serde::Serialize;
use rdev::{Event, EventType, Key};
use chrono::{DateTime, Utc};
use std::{collections::HashMap, sync::{Mutex, Arc}, time::{Duration, SystemTime, UNIX_EPOCH}};
use std::sync::mpsc;
use std::thread;

#[derive(Serialize)]
pub struct KeylogEntry {
    timestamp: String,
    keys: HashMap<String, KeyStats>,
}

#[derive(Serialize, Default, Clone)]
pub struct KeyStats {
    #[serde(skip_serializing_if = "is_zero")]
    shift: u32,
    #[serde(skip_serializing_if = "is_zero")]
    raw: u32,
    #[serde(skip_serializing_if = "is_zero")]
    bare: u32,
    #[serde(skip_serializing_if = "is_zero")]
    ctrl: u32,
    #[serde(skip_serializing_if = "is_zero")]
    alt: u32,
    #[serde(rename = "ctrl+shift", skip_serializing_if = "is_zero")]
    ctrl_shift: u32,
    #[serde(rename = "ctrl+alt", skip_serializing_if = "is_zero")]
    ctrl_alt: u32,
    #[serde(rename = "shift+alt", skip_serializing_if = "is_zero")]
    shift_alt: u32,
}

fn is_zero(val: &u32) -> bool {
    *val == 0
}

fn unix_now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
}

pub struct KeyAggregator {
    current_interval: u128,
    interval_length: Duration,
    interval_start: DateTime<Utc>,
    key_data: HashMap<String, KeyStats>,
    shift_pressed: bool,
    ctrl_pressed: bool,
    alt_pressed: bool,
}

impl KeyAggregator {
    pub fn new(interval_length: Duration) -> Self {
        Self {
            current_interval: unix_now().as_nanos() / interval_length.as_nanos(),
            interval_length,
            interval_start: Utc::now(),
            key_data: HashMap::new(),
            shift_pressed: false,
            ctrl_pressed: false,
            alt_pressed: false,
        }
    }

    pub fn start_keylogger_processor(mut self) -> (Arc<Mutex<Self>>, mpsc::Receiver<KeylogEntry>) {
        let (tx, rx) = mpsc::channel();
        let interval_length = self.interval_length;

        let processor = Arc::new(Mutex::new(self));
        let processor_clone = Arc::clone(&processor);

        thread::spawn(move || {
            loop {
                thread::sleep(interval_length);

                let entry = {
                    let mut proc = processor_clone.lock().unwrap();
                    let current_time = unix_now();
                    let current_interval = current_time.as_nanos() / interval_length.as_nanos();
                    proc.flush_interval(current_time.as_nanos(), current_interval)
                };

                if let Some(entry) = entry {
                    if tx.send(entry).is_err() {
                        break;
                    }
                }
            }
        });

        (processor, rx)
    }

    pub fn force_flush(&mut self) -> Option<KeylogEntry> {
        let current_time = unix_now();
        let current_interval: u128 = current_time.as_nanos() / self.interval_length.as_nanos();


        if current_interval != self.current_interval {
            self.flush_interval(current_time.as_nanos(), current_interval)
        } else {
            None
        }
    }

    fn flush_interval(&mut self, current_time: u128, current_interval: u128) -> Option<KeylogEntry> {
        let entry = KeylogEntry {
            timestamp: self.interval_start.to_rfc3339(),
            keys: self.key_data.clone(), // Include even if empty
        };

        self.key_data.clear();
        self.current_interval = current_interval;

        let secs = (current_time / 1_000_000_000) as i64;
        let subsec_nanos = (current_time % 1_000_000_000) as u32;
        if let Some(datetime) = DateTime::from_timestamp(secs, subsec_nanos) {
            self.interval_start = datetime;
        } else {
            self.interval_start = Utc::now();
        }

        Some(entry)
    }


    pub fn process_event(&mut self, event: Event) -> Option<KeylogEntry> {
        let current_time = unix_now();
        let current_interval: u128 = current_time.as_nanos() / self.interval_length.as_nanos();
        let should_flush = current_interval != self.current_interval;

        match event.event_type {
            EventType::KeyPress(key) => {
                self.update_modifiers(&key, true);
                if !self.is_modifier(&key) {
                    self.record_key(&key);
                }
            }
            EventType::KeyRelease(key) => {
                self.update_modifiers(&key, false);
            }
            _ => {}
        }

        if should_flush {
            self.flush_interval(current_time.as_nanos(), current_interval)
        } else {
            None
        }
    }

    fn key_to_string(key: &Key) -> String {
        let json = serde_json::to_string(key).unwrap_or_else(|_| format!("{:?}", key));

        let mut key_name = json.trim_matches('"').to_lowercase();

        if key_name.starts_with("key") && key_name.len() > 3 {
            key_name = key_name[3..].to_string();
        }

        key_name
    }

    fn is_modifier(&self, key: &Key) -> bool {
        matches!(
            key,
            Key::ShiftLeft
                | Key::ShiftRight
                | Key::ControlLeft
                | Key::ControlRight
                | Key::Alt
                | Key::AltGr
        )
    }

    fn update_modifiers(&mut self, key: &Key, pressed: bool) {
        match key {
            Key::ShiftLeft | Key::ShiftRight => self.shift_pressed = pressed,
            Key::ControlLeft | Key::ControlRight => self.ctrl_pressed = pressed,
            Key::Alt | Key::AltGr => self.alt_pressed = pressed,
            _ => {}
        }
    }

    fn record_key(&mut self, key: &Key) {
        let key_name = Self::key_to_string(key);
        let stats = self.key_data.entry(key_name).or_default();

        stats.raw += 1;

        let modifier_count =
            self.shift_pressed as u8 + self.ctrl_pressed as u8 + self.alt_pressed as u8;

        match modifier_count {
            0 => stats.bare += 1,
            1 => {
                if self.shift_pressed {
                    stats.shift += 1;
                } else if self.ctrl_pressed {
                    stats.ctrl += 1;
                } else if self.alt_pressed {
                    stats.alt += 1;
                }
            }
            2 => {
                if self.shift_pressed && self.ctrl_pressed {
                    stats.ctrl_shift += 1;
                } else if self.ctrl_pressed && self.alt_pressed {
                    stats.ctrl_alt += 1;
                } else if self.shift_pressed && self.alt_pressed {
                    stats.shift_alt += 1;
                }
            }
            _ => {
                // TODO: think should i here anything?
            }
        }
    }

    pub fn finalize(&mut self) -> Option<KeylogEntry> {
        if self.key_data.is_empty() {
            return None;
        }

        Some(KeylogEntry {
            timestamp: self.interval_start.to_rfc3339(),
            keys: self.key_data.clone()
        })
    }
}
