//! Deferred removal of files that a running process still holds open.
//!
//! Windows locks the image of a running process, so the binary being updated or
//! uninstalled cannot be overwritten or deleted while it executes. Renaming it aside
//! is permitted, which frees the original name for writing, but the renamed file stays
//! locked until the owning process exits.
//!
//! `MoveFileEx` with `MOVEFILE_DELAY_UNTIL_REBOOT` would defer that delete to the OS,
//! but it needs administrator rights and records work in
//! `HKLM\...\PendingFileRenameOperations`, which is unsuitable for a per-user tool.
//! `FileDispositionInfoEx` with POSIX semantics would allow deleting an open image but
//! is only honoured on Windows 10 1607+ over NTFS, so it cannot be relied on.
//!
//! Instead Rake re-executes itself with a hidden internal flag. The helper waits on the
//! owning process handle and then removes the file.

#![allow(unsafe_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::Result;

/// Hidden flag that puts a freshly spawned Rake into cleanup mode.
pub const INTERNAL_FLAG: &str = "--internal-deferred-delete";

#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Arguments understood by the cleanup helper, produced by [`schedule_delete`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupRequest {
    /// File to delete once `owner_pid` has exited.
    pub target: PathBuf,
    /// Directory to remove if deleting `target` leaves it empty.
    pub prune_dir: Option<PathBuf>,
    /// Process holding the lock on `target`.
    pub owner_pid: u32,
    /// Remove the helper's own image on completion. Set when the helper had to be
    /// copied aside because the only executable available was the locked file.
    pub self_delete: bool,
}

/// Block until `pid` has exited.
///
/// On Windows this waits on the process handle, so it returns as soon as the owner
/// exits rather than after a fixed delay. If the handle cannot be opened the process is
/// already gone and there is nothing to wait for.
#[cfg(windows)]
pub fn wait_for_process(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    const INFINITE: u32 = u32::MAX;

    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return;
        }
        WaitForSingleObject(handle, INFINITE);
        let _ = CloseHandle(handle);
    }
}

/// Unix keeps no mandatory lock on a running executable, so there is nothing to wait for.
#[cfg(not(windows))]
pub fn wait_for_process(_pid: u32) {}

/// Delete `target`, then remove `prune_dir` if it is now empty.
///
/// Missing files are treated as success: the point of the helper is to guarantee the
/// file is gone, not to report that something was there to begin with.
///
/// After pruning, one level above is considered as well. Uninstall names the bin
/// directory, but the install root above it is left holding nothing; removing it keeps
/// the promise that uninstalling leaves nothing behind. The walk stops after that one
/// level so cleanup can never climb past the directory the caller named.
pub fn remove_and_prune(target: &Path, prune_dir: Option<&Path>) -> Result<()> {
    match std::fs::remove_file(target) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }

    let Some(dir) = prune_dir else {
        return Ok(());
    };

    if !is_empty_dir(dir)? {
        return Ok(());
    }
    remove_dir_if_empty(dir)?;

    if let Some(parent) = dir.parent()
        && is_empty_dir(parent)?
    {
        remove_dir_if_empty(parent)?;
    }

    Ok(())
}

fn is_empty_dir(dir: &Path) -> Result<bool> {
    if !dir.is_dir() {
        return Ok(false);
    }
    Ok(std::fs::read_dir(dir)?.next().is_none())
}

fn remove_dir_if_empty(dir: &Path) -> Result<()> {
    match std::fs::remove_dir(dir) {
        Ok(()) => Ok(()),
        // Losing a race with another writer is not a failure worth reporting.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Spawn a detached Rake that performs the cleanup described by `request`.
///
/// `helper_exe` is the executable to run. It must not be `request.target`: a process
/// cannot delete the image it is executing from, so when the only binary at hand is the
/// locked file it is copied to a temporary location first and the helper is told to
/// remove that copy as its last act.
pub fn spawn_cleanup_helper(helper_exe: &Path, request: &CleanupRequest) -> Result<()> {
    let mut args: Vec<OsString> = vec![
        OsString::from(INTERNAL_FLAG),
        OsString::from("--wait-pid"),
        OsString::from(request.owner_pid.to_string()),
        OsString::from("--target"),
        request.target.as_os_str().to_owned(),
    ];

    if let Some(dir) = &request.prune_dir {
        args.push(OsString::from("--prune-dir"));
        args.push(dir.as_os_str().to_owned());
    }
    if request.self_delete {
        args.push(OsString::from("--self-delete"));
    }

    spawn_detached(helper_exe, &args)
}

/// Run the cleanup helper body: wait for the owner, delete, prune, tidy up after self.
pub fn run_cleanup_helper(request: &CleanupRequest) -> Result<()> {
    wait_for_process(request.owner_pid);
    remove_and_prune(&request.target, request.prune_dir.as_deref())?;

    if request.self_delete
        && let Ok(exe) = std::env::current_exe()
    {
        schedule_own_removal(&exe);
    }

    Ok(())
}

/// Delete the helper's own image after it exits.
///
/// The helper is running from this file, so it cannot unlink it directly. Handing the
/// job to `cmd.exe` works because cmd is a different image and is not locked. The loop
/// retries because cmd usually starts while this process is still exiting, and a fixed
/// sleep would either be too short or needlessly delay every case.
#[cfg(windows)]
fn schedule_own_removal(exe: &Path) {
    use std::os::windows::process::CommandExt;

    let script = format!(
        "for /L %i in (1,1,30) do (@del /f /q \"{0}\" 2>nul & @if not exist \"{0}\" @exit /b 0 & @ping -n 2 127.0.0.1 >nul)",
        exe.display()
    );

    let mut cmd = std::process::Command::new("cmd");
    // The script is cmd syntax, not a C runtime argument list: escaping its quotes the
    // usual way would hand cmd backslashes it does not understand.
    cmd.arg("/c")
        .raw_arg(&script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    let _ = cmd.spawn();
}

#[cfg(not(windows))]
fn schedule_own_removal(_exe: &Path) {}

/// Build the cleanup request for `target` and launch the helper.
///
/// `helper_exe` is preferred, but when it resolves to `target` itself the binary is
/// copied to a temporary file first so the helper is not executing from the locked image.
pub fn schedule_delete(
    target: &Path,
    prune_dir: Option<&Path>,
    owner_pid: u32,
    helper_exe: &Path,
) -> Result<CleanupRequest> {
    // A helper that is missing, or that resolves to the locked file itself, cannot be
    // used: the first cannot be executed and the second would be deleting its own image.
    // Copying the locked binary to a temporary path solves both, and reading a running
    // image is permitted even though writing and deleting it are not.
    let (exe, self_delete) = if helper_exe.is_file() && !same_file(helper_exe, target) {
        (helper_exe.to_path_buf(), false)
    } else {
        (stage_helper(target, owner_pid)?, true)
    };

    let request = CleanupRequest {
        target: target.to_path_buf(),
        prune_dir: prune_dir.map(Path::to_path_buf),
        owner_pid,
        self_delete,
    };

    spawn_cleanup_helper(&exe, &request)?;
    Ok(request)
}

/// Copy the locked binary somewhere runnable.
///
/// Reading a running image is permitted — only writing and deleting are refused — so
/// this works even though the source cannot be modified.
fn stage_helper(target: &Path, owner_pid: u32) -> Result<PathBuf> {
    let staged = std::env::temp_dir().join(format!("rake-cleanup-{owner_pid}.exe"));
    std::fs::copy(target, &staged)?;
    Ok(staged)
}

/// Compare paths by identity where possible, falling back to a literal comparison.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Start `exe` fully detached so it survives this process exiting.
fn spawn_detached(exe: &Path, args: &[OsString]) -> Result<()> {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    // Dropping the handle without waiting detaches the child: it keeps running after
    // this process exits, which is exactly what the deferred cleanup needs.
    drop(cmd.spawn()?);
    Ok(())
}
