use super::ArchiveFormat;

/// Detect an archive format from a file name.
///
/// The input is the file's name on disk — for a manifest URL that is
/// `url_filename(url)`, i.e. the `#/name.ext` fragment when present. This is
/// exactly what Scoop's `Invoke-Extraction` switches on (`$Name[$i]`,
/// lib/decompress.ps1:25), so `Setup.exe#/dl.7z` is a 7-Zip archive and a bare
/// `tool.exe` is not.
///
/// Two deliberate omissions, both copied from Scoop:
/// * `.cab` is **not** an archive. Main-bucket manifests (`msival2`, `orca`,
///   `visualc`) download `.cab` files next to an `.msi`; `msiexec /a` resolves
///   external cabinets relative to the `.msi`, so the cabinets have to stay on
///   disk. Extracting them breaks those packages.
/// * `.jar` is **not** an archive either. `plantuml` ships a `.jar` that Scoop
///   copies into the version directory and runs; unzipping it would destroy it.
pub fn detect_format(path: &str) -> Option<ArchiveFormat> {
    let lower = path.to_lowercase();
    let path = lower.split('?').next().unwrap_or(&lower);
    // Compound extensions first: `.tar.gz` must not be mistaken for `.gz`.
    if path.ends_with(".zip") || path.ends_with(".nupkg") {
        Some(ArchiveFormat::Zip)
    } else if path.ends_with(".7z") {
        Some(ArchiveFormat::SevenZ)
    } else if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
        Some(ArchiveFormat::TarGz)
    } else if path.ends_with(".tar.bz2") || path.ends_with(".tbz2") || path.ends_with(".tbz") {
        Some(ArchiveFormat::TarBz2)
    } else if path.ends_with(".tar.xz") || path.ends_with(".txz") {
        Some(ArchiveFormat::TarXz)
    } else if path.ends_with(".tar") {
        Some(ArchiveFormat::Tar)
    } else if path.ends_with(".gz") {
        Some(ArchiveFormat::Gz)
    } else if path.ends_with(".xz") && !path.ends_with(".tar.xz") {
        Some(ArchiveFormat::Xz)
    } else if path.ends_with(".bz2") || path.ends_with(".bzip2") {
        Some(ArchiveFormat::Bz2)
    } else if path.ends_with(".msi") {
        Some(ArchiveFormat::Msi)
    } else if path.ends_with(".rar") {
        Some(ArchiveFormat::Rar)
    } else if path.ends_with(".zst")
        || path.ends_with(".tzst")
        || path.ends_with(".lzma")
        || path.ends_with(".lzh")
        || path.ends_with(".iso")
        || path.ends_with(".img")
        || path.ends_with(".001")
    {
        Some(ArchiveFormat::External7z)
    } else {
        None
    }
}

/// Determine the archive format to use for extraction, honoring
/// Scoop's `url#/filename.ext` convention: some manifests append a URL
/// fragment (e.g. `#/dl.7z`) to declare a download's *true* archive
/// format when its literal HTTP extension doesn't reflect it. A common
/// real case: Git for Windows ships PortableGit as a self-extracting
/// `.exe` that is actually a `.7z` archive under the hood. When a
/// fragment with a recognizable extension is present, it is
/// authoritative for extraction purposes — never fall back to
/// re-deriving the format from a cache file's own path, since caching
/// logic may have already stripped this fragment.
pub fn detect_format_for_url(url: &str) -> Option<ArchiveFormat> {
    if let Some((_, fragment)) = url.split_once('#')
        && let Some(fmt) = detect_format(fragment)
    {
        return Some(fmt);
    }
    let base = url.split('#').next().unwrap_or(url);
    detect_format(base)
}
