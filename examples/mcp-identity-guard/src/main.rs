use mcp_identity_guard::{Result, deny, lock, proxy, read_manifest, verify_ledger};
use serde_json::json;
use std::path::Path;

mod evaluation;
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("lock") if args.len() == 4 => {
            let locked = lock(read_manifest(Path::new(&args[2]))?)?;
            // Never overwrite a reviewed lock implicitly.
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&args[3])
                .map_err(|_| {
                    deny(
                        "LOCK_OUTPUT",
                        "operator",
                        "output must be new writable file",
                        "Choose a new output path and review diff.",
                        false,
                    )
                })?;
            f.write_all(&serde_json::to_vec_pretty(&locked).unwrap())
                .map_err(|_| {
                    deny(
                        "IO",
                        "operator",
                        "write failed",
                        "Inspect output file before retrying.",
                        false,
                    )
                })?;
            Ok(())
        }
        Some("proxy") if args.len() == 4 => {
            proxy(read_manifest(Path::new(&args[2]))?, Path::new(&args[3]))
        }
        Some("verify-ledger") if args.len() == 3 => {
            let (records, head) = verify_ledger(Path::new(&args[2]))?;
            println!("{}", json!({"records":records,"head":head}));
            Ok(())
        }
        Some("bench") if args.len() == 2 => {
            println!(
                "{}",
                serde_json::to_string_pretty(&evaluation::benchmark()?).unwrap()
            );
            Ok(())
        }
        Some("oracle-bench") if args.len() == 2 => {
            println!(
                "{}",
                serde_json::to_string_pretty(&evaluation::oracle_benchmark()?).unwrap()
            );
            Ok(())
        }
        Some("__mock") => evaluation::mock(args.get(2).map(String::as_str).unwrap_or("a")),
        _ => Err(deny(
            "USAGE",
            "cli",
            "invalid arguments",
            "Use lock SPEC NEW_LOCK | proxy LOCK LEDGER | verify-ledger LEDGER | bench | oracle-bench.",
            true,
        )),
    }
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
