use std::io::Read;
use std::io::Write;

pub fn main() -> ! {
    let exit_code = run_main();
    std::process::exit(exit_code);
}

pub fn run_main() -> i32 {
    let mut args = std::env::args_os();
    let _argv0 = args.next();

    let patch_arg = match args.next() {
        Some(arg) => match arg.into_string() {
            Ok(s) => s,
            Err(_) => {
                eprintln!("Error: apply_patch requires a UTF-8 PATCH argument.");
                return 1;
            }
        },
        None => {
            let mut buf = String::new();
            match std::io::stdin().read_to_string(&mut buf) {
                Ok(_) => {
                    if buf.is_empty() {
                        eprintln!("Usage: apply_patch 'PATCH'\n       echo 'PATCH' | apply_patch");
                        return 2;
                    }
                    buf
                }
                Err(err) => {
                    eprintln!("Error: Failed to read PATCH from stdin.\n{err}");
                    return 1;
                }
            }
        }
    };

    if args.next().is_some() {
        eprintln!("Error: apply_patch accepts exactly one argument.");
        return 2;
    }

    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let cwd = match octopus_util_abspath::AbsolutePathBuf::current_dir() {
        Ok(cwd) => cwd,
        Err(err) => {
            eprintln!("Error: Failed to determine current directory.\n{err}");
            return 1;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("Error: Failed to initialize runtime.\n{err}");
            return 1;
        }
    };
    let cwd = octopus_util_pathuri::PathUri::from_abs_path(&cwd);
    let update_file_mode = crate::apply_patch_file_update_mode_from_env();
    match runtime.block_on(crate::apply_patch_with_options(
        &patch_arg,
        crate::ApplyPatchOptions {
            update_file_mode,
            ..Default::default()
        },
        &cwd,
        &mut stdout,
        &mut stderr,
        octopus_file_system::LOCAL_FS.as_ref(),
        None,
    )) {
        Ok(_) => {
            let _ = stdout.flush();
            0
        }
        Err(_) => 1,
    }
}
