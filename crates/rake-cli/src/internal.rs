//! Hidden helper mode used for deferred cleanup of a locked binary.
//!
//! Spawned by `infra::self_replace` when Rake replaces or removes itself. It must run
//! before any session setup: cleanup has to work even when the configuration is broken,
//! and it must stay cheap — no config read, no tracing, no network.

use std::path::PathBuf;

use rake_core::infra::self_replace::{self, CleanupRequest, INTERNAL_FLAG};

/// Run the cleanup helper if `args` invoke it, reporting whether it was handled.
///
/// `args[0]` is the executable path, so the flag is expected at index 1.
pub fn dispatch(args: &[String]) -> bool {
    if args.get(1).map(String::as_str) != Some(INTERNAL_FLAG) {
        return false;
    }

    if let Err(e) = run(args) {
        // Nothing useful can be reported here: stdout is redirected to null so the
        // helper never disturbs the parent's console output.
        tracing::debug!("deferred cleanup failed: {e}");
        std::process::exit(1);
    }
    true
}

fn run(args: &[String]) -> anyhow::Result<()> {
    let mut request = CleanupRequest {
        target: PathBuf::new(),
        prune_dir: None,
        owner_pid: 0,
        self_delete: false,
    };

    // Skip the executable path and the internal flag itself.
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--target" => request.target = PathBuf::from(next(args, &mut i)?),
            "--prune-dir" => request.prune_dir = Some(PathBuf::from(next(args, &mut i)?)),
            "--wait-pid" => {
                let raw = next(args, &mut i)?;
                request.owner_pid = raw
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid --wait-pid value: {raw}"))?;
            }
            "--self-delete" => request.self_delete = true,
            other => anyhow::bail!("unknown argument: {other}"),
        }
        i += 1;
    }

    if request.target.as_os_str().is_empty() {
        anyhow::bail!("--target is required");
    }
    if request.owner_pid == 0 {
        anyhow::bail!("--wait-pid is required");
    }

    self_replace::run_cleanup_helper(&request)?;
    Ok(())
}

fn next(args: &[String], i: &mut usize) -> anyhow::Result<String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing value for {}", args[*i - 1]))
}
