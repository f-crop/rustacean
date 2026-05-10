use std::path::Path;

use anyhow::{Context as _, Result};

pub fn extract_tar_zst(data: &[u8], dest: &Path) -> Result<()> {
    let decoded = zstd::decode_all(std::io::Cursor::new(data))
        .context("failed to decompress zstd archive")?;
    let mut archive = tar::Archive::new(std::io::Cursor::new(decoded));
    archive
        .unpack(dest)
        .context("failed to unpack tar archive")?;
    Ok(())
}
