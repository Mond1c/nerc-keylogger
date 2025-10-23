use serde::Serialize;
use rdev::{Event, EventType, Key};
use chrono::{DateTime, Utc};
use std::{collections::HashMap, time::{SystemTime, UNIX_EPOCH}};

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

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn unix_minute_now() -> u64 {
    unix_now() / 60
}

pub struct KeyAggregator {
    current_minute: u64,
    minute_start: DateTime<Utc>,
    key_data: HashMap<String, KeyStats>,
    shift_pressed: bool,
    ctrl_pressed: bool,
    alt_pressed: bool,
}

impl KeyAggregator {
    pub fn new() -> Self {
        let now = Utc::now();
        Self {
            current_minute: unix_minute_now(),
            minute_start: now,
            key_data: HashMap::new(),
            shift_pressed: false,
            ctrl_pressed: false,
            alt_pressed: false,
        }
    }

    pub fn process_event(&mut self, event: Event) -> Option<KeylogEntry> {
        let current_minute = unix_minute_now();

        let should_flush = current_minute != self.current_minute;

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

        if should_flush && !self.key_data.is_empty() {
            let entry = KeylogEntry {
                timestamp: self.minute_start.to_rfc3339(),
                keys: self.key_data.clone(),
            };

            self.key_data.clear();
            self.current_minute = current_minute;
            self.minute_start = Utc::now();

            Some(entry)
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
            timestamp: self.minute_start.to_rfc3339(),
            keys: self.key_data.clone()
        })
    }
}
