#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{collections::HashMap, fs::File, io::{BufWriter, Read, Write}, time::{SystemTime, UNIX_EPOCH}};
use chrono::{DateTime, Utc};
use flate2::{write::GzEncoder, Compression};
use rdev::{listen, Event, EventType, Key};
use serde::Serialize;
use reqwest::Client;
use tokio::{select, sync::watch};
use std::sync::{Arc, Mutex};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Upload endpoint URL
    #[arg(short, long, default_value = "http://127.0.0.1:8080/api/upload")]
    url: String,

    /// Output file path (without .current suffix)
    #[arg(short, long, default_value = "keylog.ndjson")]
    output: String,

    /// Upload interval in seconds
    #[arg(short, long, default_value_t = 60)]
    interval: u64,

    /// Enable debug mode with periodic uploads
    #[arg(short, long, default_value_t = false)]
    debug: bool,
}

#[derive(Serialize)]
struct KeylogEntry {
    timestamp: String,
    keys: HashMap<String, KeyStats>,
}

#[derive(Serialize, Default, Clone)]
struct KeyStats {
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

struct KeyAggregator {
    current_minute: u64,
    minute_start: DateTime<Utc>,
    key_data: HashMap<String, KeyStats>,
    shift_pressed: bool,
    ctrl_pressed: bool,
    alt_pressed: bool,
}

impl KeyAggregator {
    fn new() -> Self {
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

    fn process_event(&mut self, event: Event) -> Option<KeylogEntry> {
        let current_minute = unix_minute_now();

        let should_flush = if current_minute != self.current_minute {
            true
        } else {
            false
        };

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
        let key_name = format!("{:?}", key).to_lowercase();
        let stats = self.key_data.entry(key_name).or_insert_with(KeyStats::default);

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

    fn finalize(&mut self) -> Option<KeylogEntry> {
        if self.key_data.is_empty() {
            return None;
        }

        Some(KeylogEntry {
            timestamp: self.minute_start.to_rfc3339(),
            keys: self.key_data.clone()
        })
    }
}

fn unix_now() -> u64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    secs
}

fn unix_minute_now() -> u64 {
    unix_now() / 60
}


fn append_entry<W>(
    writer: &mut W,
    entry: &KeylogEntry,
) -> std::io::Result<()>
where
    W: Write
{
    let value = serde_json::to_string(&entry)?;
    writer.write_all(format!("{}\n", value).as_bytes())?;
    Ok(())
}

fn create_callback(
    mut writer: BufWriter<File>,
    flush_signal: Arc<Mutex<bool>>
) -> Box<dyn FnMut(Event) + Send> {
    let mut aggregator = KeyAggregator::new();

    let cb = move |event: Event| {
        if let Some(entry) = aggregator.process_event(event) {
            if let Err(e) = append_entry(&mut writer, &entry) {
                eprintln!("Failed to write to file: {}", e);
            }

            if let Err(e) = writer.flush() {
                eprintln!("Failed to flush file: {}", e);
            }

            if let Ok(mut signal) = flush_signal.lock() {
                *signal = true;
            }
        }
    };

    Box::new(cb)
}

fn gzip_file_to_vec(path: &str) -> anyhow::Result<Vec<u8>> {
    let mut input = File::open(path)?;
    let mut buf = Vec::new();

    input.read_to_end(&mut buf)?;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&buf)?;
    let compressed = encoder.finish()?;
    Ok(compressed)
}

async fn upload_keylog(
    url: &str,
    path: &str,
) -> anyhow::Result<()> {
    let compressed = gzip_file_to_vec(path)?;

    let form = reqwest::multipart::Form::new()
        .part("file", reqwest::multipart::Part::bytes(compressed)
            .file_name("keylog.ndjson.gz")
            .mime_str("application/gzip")?);

    let response = Client::new()
        .post(url)
        .multipart(form)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Failed to upload file: {}",
            response.status()
        ));
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let current_file = format!("{}.current", args.output);
    let file = File::options()
        .create(true)
        .append(true)
        .open(&current_file)?;
    let writer = BufWriter::new(file);

    println!("Keylogger started");
    println!("Output file: {}", current_file);
    if args.debug {
        println!("Debug mode: enabled");
        println!("Upload URL: {}", args.url);
        println!("Upload interval: {} seconds", args.interval);
    }

    let flush_signal = Arc::new(Mutex::new(false));
    let callback = create_callback(writer, flush_signal.clone());
    let (stop_tx, mut stop_rx) = watch::channel(false);

    if args.debug {
        let url = args.url.clone();
        let path = args.output.clone();
        let current_file_clone = current_file.clone();

        let send_thread = tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(args.interval));

            loop {
                select! {
                    _ = tick.tick() => {
                        let should_upload = if let Ok(mut signal) = flush_signal.lock() {
                            let should = *signal
                                && std::path::Path::new(&current_file_clone)
                                    .metadata()
                                    .map(|m| m.len())
                                    .unwrap_or(0)
                                    > 0;
                            *signal = false;
                            should
                        } else {
                            false
                        };

                        if should_upload {
                            println!("Preparing to upload keylog...");
                            if let Err(e) = std::fs::copy(&current_file_clone, &path) {
                                eprintln!("Failed to copy current file: {}", e);
                            } else if let Err(error) = upload_keylog(&url, &path).await {
                                eprintln!("Upload error: {:?}", error);
                            }
                        }
                    }
                    _ = stop_rx.changed() => {
                        if *stop_rx.borrow() {
                            println!("Stopping upload thread...");
                            break;
                        }
                    }
                }
            }
        });

        if let Err(error) = listen(callback) {
            eprintln!("Listen error: {:?}", error);
        }
        stop_tx.send(true)?;
        let _ = send_thread.await?;
    } else {
        if let Err(error) = listen(callback) {
            eprintln!("Listen error: {:?}", error);
        }
    }

    println!("Keylogger stopped");
    Ok(())
}
