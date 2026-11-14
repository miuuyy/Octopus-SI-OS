#[cfg(target_os = "linux")]
mod peer;
#[cfg(target_os = "linux")]
mod service;

fn main() {
    #[cfg(target_os = "linux")]
    if let Err(error) = run() {
        eprintln!("Ubuntu process authority: {error}");
        std::process::exit(1);
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("Ubuntu process authority requires Linux and AppArmor");
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
fn run() -> Result<(), String> {
    use octopus_verifiers::process_authority::linux::{prepare_room, LinuxRoomPolicy};
    use std::os::unix::process::CommandExt;
    let mut args = std::env::args_os().skip(1);
    match args.next() {
        None => service::serve(),
        Some(operation) if operation == "exec" => {
            if unsafe { libc::geteuid() } == 0 {
                return Err("room commands must run as the ordinary Octopus user".into());
            }
            let policy = args.next().ok_or("room policy is absent")?;
            let policy: LinuxRoomPolicy =
                serde_json::from_str(policy.to_str().ok_or("room policy is not UTF-8")?)
                    .map_err(|error| format!("invalid room policy: {error}"))?;
            if args.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
                return Err("room command requires '--' after its policy".into());
            }
            let argv = args.collect::<Vec<_>>();
            let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
            let prepared = prepare_room(policy, &argv, &cwd, std::env::var_os("PATH").as_deref())?;
            let error = prepared.command().exec();
            Err(format!(
                "room command could not enter its process authority: {error}"
            ))
        }
        Some(_) => {
            Err("usage: octopus-linux-process-authority [exec POLICY -- COMMAND ...]".into())
        }
    }
}
