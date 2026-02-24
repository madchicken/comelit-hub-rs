use anyhow::{Context, Result};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use tracing::{debug, info};

/// Audio stream assembler
pub(crate) struct AudioAssembler {
    packet_count: usize,
    output_file: File,
}

impl AudioAssembler {
    pub fn new(output_path: &PathBuf) -> Result<Self> {
        let output_file = File::create(output_path).context(format!(
            "Failed to create audio output file: {:?}",
            output_path
        ))?;

        Ok(Self {
            packet_count: 0,
            output_file,
        })
    }

    pub fn process_packet(&mut self, payload: &[u8]) -> Result<()> {
        // For audio, just write raw payload (PCM, G.711, AAC, etc.)
        self.output_file.write_all(payload)?;
        self.packet_count += 1;

        if self.packet_count % 100 == 0 {
            debug!("Written {} audio packets", self.packet_count);
        }

        Ok(())
    }

    pub fn finalize(&mut self) -> Result<usize> {
        self.output_file.flush()?;
        info!("Total audio packets written: {}", self.packet_count);
        Ok(self.packet_count)
    }
}
