use crate::collector::RequestTelemetry;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::Mutex;

pub struct WAL {
    writer: Mutex<BufWriter<File>>,
    path: String,
}

impl WAL {
    pub fn new(path: &str) -> Self {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("WAL: failed to open file");
        let writer = BufWriter::new(file);
        WAL {
            writer: Mutex::new(writer),
            path: path.to_string(),
        }
    }

    pub fn append(&self, tele: &RequestTelemetry) -> std::io::Result<()> {
        let line = serde_json::to_string(tele).unwrap_or_default();
        let mut w = self.writer.lock().unwrap();
        writeln!(w, "{}", line)?;
        Ok(())
    }

    pub fn flush(&self) -> std::io::Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.flush()?;
        let file = w.get_mut();
        file.sync_all()?;
        Ok(())
    }

    pub fn replay(&self) -> Vec<RequestTelemetry> {
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        let reader = BufReader::new(file);
        let mut result = Vec::new();
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(tele) = serde_json::from_str::<RequestTelemetry>(&line) {
                result.push(tele);
            }
        }
        result
    }

    pub fn archive(&self) -> std::io::Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.flush()?;
        let file = w.get_mut();
        file.sync_all()?;

        let result = std::process::Command::new("gzip")
            .arg("-f")
            .arg(&self.path)
            .status();

        match result {
            Ok(status) if status.success() => {
                let new_file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?;
                *w = BufWriter::new(new_file);
                Ok(())
            }
            Ok(_) => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "gzip failed",
            )),
            Err(e) => Err(e),
        }
    }
}
