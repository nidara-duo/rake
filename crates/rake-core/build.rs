use std::path::{Path, PathBuf};
use std::process::Command;

// Builds the shim binary and drops it next to this crate's OUT_DIR so that
// `infra::shim` can embed it with `include_bytes!`.
//
// Why a nested cargo invocation instead of a normal dependency: the shim is a
// separate crate in this workspace (`rake-shim-bin`), and the workspace's
// `default-members` is `["crates/rake-cli"]`, so a plain `cargo build` never
// compiles it. Embedding its bytes requires the finished artifact to exist
// *before* this crate is compiled, which a build script is the only place to
// arrange. Cargo gives no ordering guarantee between workspace members, so
// building it "in parallel" would be a race.
//
// The nested build gets its own CARGO_TARGET_DIR deliberately: the outer cargo
// holds an exclusive lock on the workspace target directory for the whole
// build, including while build scripts run. Pointing the inner cargo at the
// same directory would deadlock.

fn main() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is always set"));
    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crate lives at <workspace>/crates/<name>");
    let target = std::env::var("TARGET").expect("TARGET is always set");

    println!("cargo:rerun-if-env-changed=RAKE_SHIM_BIN");
    println!("cargo:rerun-if-changed=../rake-shim-bin/src");
    println!("cargo:rerun-if-changed=../rake-shim-bin/Cargo.toml");

    let dest = out_dir.join("shim.exe");

    // Escape hatch: use an already-built shim and skip the nested build. Useful
    // when iterating on rake itself without paying for the shim rebuild.
    if let Some(prebuilt) = std::env::var_os("RAKE_SHIM_BIN") {
        let prebuilt = PathBuf::from(prebuilt);
        std::fs::copy(&prebuilt, &dest).unwrap_or_else(|e| {
            panic!(
                "failed to copy RAKE_SHIM_BIN {} -> {}: {e}",
                prebuilt.display(),
                dest.display()
            )
        });
        return;
    }

    let nested_target_dir = out_dir.join("shim-build");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let status = Command::new(&cargo)
        .current_dir(workspace_root)
        .args([
            "build",
            "--release",
            "-p",
            "rake-shim-bin",
            "--target",
            &target,
        ])
        .env("CARGO_TARGET_DIR", &nested_target_dir)
        // Silence the nested build's own output unless something goes wrong;
        // otherwise every rake build prints a confusing duplicate log.
        .env("CARGO_TERM_QUIET", "true")
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn {cargo:?}: {e}"));

    assert!(
        status.success(),
        "nested build of rake-shim-bin for target {target} failed ({status})"
    );

    let built = nested_target_dir
        .join(&target)
        .join("release")
        .join("shim.exe");
    std::fs::copy(&built, &dest).unwrap_or_else(|e| {
        panic!(
            "failed to copy {} -> {}: {e}",
            built.display(),
            dest.display()
        )
    });
}
