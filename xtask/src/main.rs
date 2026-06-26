//! xtask — repo automation binary. Exists so the "thiserror in libraries,
//! anyhow at binaries" rule (invariant #10) is concrete: this is a binary, so
//! it uses `anyhow`; `droplet-core` is a library, so it never does.

fn main() -> anyhow::Result<()> {
    println!("xtask: nothing to do yet");
    Ok(())
}
