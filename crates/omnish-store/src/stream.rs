use anyhow::Result;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, Write};
use std::path::Path;

/// Binary format per entry: timestamp_ms(8) + direction(1) + data_len(4) + data(N)
pub struct StreamWriter {
    writer: BufWriter<File>,
    pos: u64,
}

pub struct StreamEntry {
    pub timestamp_ms: u64,
    pub direction: u8,
    pub data: Vec<u8>,
}

impl StreamWriter {
    pub fn create(path: &Path) -> Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            pos: 0,
        })
    }

    pub fn position(&self) -> u64 {
        self.pos
    }

    pub fn write_entry(&mut self, timestamp_ms: u64, direction: u8, data: &[u8]) -> Result<()> {
        self.writer.write_all(&timestamp_ms.to_be_bytes())?;
        self.writer.write_all(&[direction])?;
        self.writer.write_all(&(data.len() as u32).to_be_bytes())?;
        self.writer.write_all(data)?;
        self.writer.flush()?;
        self.pos += 8 + 1 + 4 + data.len() as u64;
        Ok(())
    }
}

pub fn read_range(path: &Path, offset: u64, length: u64) -> Result<Vec<StreamEntry>> {
    let mut file = File::open(path)?;
    file.seek(std::io::SeekFrom::Start(offset))?;
    let mut data = vec![0u8; length as usize];
    file.read_exact(&mut data)?;

    let mut entries = Vec::new();
    let mut pos = 0;
    while pos + 13 <= data.len() {
        let timestamp_ms = u64::from_be_bytes(data[pos..pos + 8].try_into()?);
        let direction = data[pos + 8];
        let data_len = u32::from_be_bytes(data[pos + 9..pos + 13].try_into()?) as usize;
        if pos + 13 + data_len > data.len() {
            break;
        }
        let entry_data = data[pos + 13..pos + 13 + data_len].to_vec();
        entries.push(StreamEntry {
            timestamp_ms,
            direction,
            data: entry_data,
        });
        pos += 13 + data_len;
    }
    Ok(entries)
}

pub fn read_entries(path: &Path) -> Result<Vec<StreamEntry>> {
    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;
    let mut entries = Vec::new();
    let mut pos = 0;
    while pos + 13 <= data.len() {
        let timestamp_ms = u64::from_be_bytes(data[pos..pos + 8].try_into()?);
        let direction = data[pos + 8];
        let data_len = u32::from_be_bytes(data[pos + 9..pos + 13].try_into()?) as usize;
        if pos + 13 + data_len > data.len() {
            break;
        }
        let entry_data = data[pos + 13..pos + 13 + data_len].to_vec();
        entries.push(StreamEntry {
            timestamp_ms,
            direction,
            data: entry_data,
        });
        pos += 13 + data_len;
    }
    Ok(entries)
}
