use std::path::Path;

use crate::Result;

pub(super) async fn extract_tar(src: &Path, dest: &Path) -> Result<()> {
    let src = src.to_path_buf();
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::open(&src)?;
        let reader = std::io::BufReader::new(file);

        let decompressed: Box<dyn std::io::Read> = match src
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("gz") | Some("tgz") => Box::new(flate2::read::GzDecoder::new(reader)),
            Some("bz2") | Some("tbz") | Some("tbz2") => {
                Box::new(bzip2::read::BzDecoder::new(reader))
            }
            Some("xz") | Some("txz") => Box::new(xz2::read::XzDecoder::new(reader)),
            _ => Box::new(reader),
        };

        let mut archive = tar::Archive::new(decompressed);
        archive
            .unpack(&dest)
            .map_err(|e| crate::Error::Archive(format!("tar: {e}")))?;

        Ok(())
    })
    .await
    .map_err(|e| crate::Error::Archive(format!("tar task join: {e}")))?
}

async fn extract_single_compressed_stream<R: std::io::Read + Send + 'static>(
    src: &Path,
    dest: &Path,
    kind: &'static str,
    make_decoder: impl FnOnce(std::io::BufReader<std::fs::File>) -> R + Send + 'static,
) -> Result<()> {
    let src = src.to_path_buf();
    let out_name = src
        .file_stem()
        .unwrap_or_else(|| src.file_name().unwrap_or_default())
        .to_os_string();
    let out_path = dest.join(out_name);

    tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::open(&src)?;
        let reader = std::io::BufReader::new(file);
        let mut decoder = make_decoder(reader);
        let mut out = std::fs::File::create(&out_path)?;
        std::io::copy(&mut decoder, &mut out)
            .map_err(|e| crate::Error::Archive(format!("{kind}: {e}")))?;
        Ok(())
    })
    .await
    .map_err(|e| crate::Error::Archive(format!("{kind} task join: {e}")))?
}

pub(super) async fn extract_gz(src: &Path, dest: &Path) -> Result<()> {
    extract_single_compressed_stream(src, dest, "gz", flate2::read::GzDecoder::new).await
}

pub(super) async fn extract_xz(src: &Path, dest: &Path) -> Result<()> {
    extract_single_compressed_stream(src, dest, "xz", xz2::read::XzDecoder::new).await
}

pub(super) async fn extract_bz2(src: &Path, dest: &Path) -> Result<()> {
    extract_single_compressed_stream(src, dest, "bz2", bzip2::read::BzDecoder::new).await
}
