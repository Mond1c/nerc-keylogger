use std::{fs::File, io::{BufWriter, Write}, time::{SystemTime, UNIX_EPOCH}};

use rdev::{listen, Event, EventType};

fn unix_minute_now() -> u64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    secs / 60
}

fn write_record<W: Write>(w: &mut W, minute_unix: u64, count: u32) -> std::io::Result<()> {
    w.write_all(&minute_unix.to_le_bytes())?;
    w.write_all(&count.to_le_bytes())
}

fn create_callback(mut writer: BufWriter<File>) -> Box<dyn FnMut(Event) + Send> {
    let mut current_minute = unix_minute_now();
    let mut count: u32 = 0;

    let cb = move |event: Event| {
        if let EventType::KeyPress(_) = event.event_type {
            let m = unix_minute_now();
            if m != current_minute {
                if let Err(e) = write_record(&mut writer, current_minute, count) {
                    eprintln!("Failed to write minute record: {}", e);
                }
                let _ = writer.flush();
                current_minute = m;
                count = 0;
            }
            count = count.saturating_add(1);
        }
    };
    Box::new(cb)
}

fn main() -> std::io::Result<()> {
    let file = File::options()
        .create(true)
        .append(true)
        .open("keylog.txt")?;
    let writer = BufWriter::new(file);
    let callback = create_callback(writer);

    if let Err(error) = listen(callback) {
        println!("Error: {:?}", error);
    }
    Ok(())
}
