use crate::command_safety::is_safe_command::is_safe_git_command;
use crate::command_safety::powershell_parser::PowershellParseOutcome;
use crate::command_safety::powershell_parser::parse_with_powershell_ast;
use std::path::Path;

pub fn is_safe_command_windows(command: &[String]) -> bool {
    if let Some(commands) = try_parse_powershell_command_sequence(command) {
        commands
            .iter()
            .all(|cmd| is_safe_powershell_words(cmd.as_slice()))
    } else {
        false
    }
}

fn try_parse_powershell_command_sequence(command: &[String]) -> Option<Vec<Vec<String>>> {
    let (exe, rest) = command.split_first()?;
    if is_powershell_executable(exe) {
        parse_powershell_invocation(exe, rest)
    } else {
        None
    }
}

fn parse_powershell_invocation(executable: &str, args: &[String]) -> Option<Vec<Vec<String>>> {
    if args.is_empty() {
        return None;
    }

    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        let lower = arg.to_ascii_lowercase();
        match lower.as_str() {
            "-command" | "/command" | "-c" => {
                let script = args.get(idx + 1)?;
                if idx + 2 != args.len() {
                    return None;
                }
                return parse_powershell_script(executable, script);
            }
            _ if lower.starts_with("-command:") || lower.starts_with("/command:") => {
                if idx + 1 != args.len() {
                    return None;
                }
                let script = arg.split_once(':')?.1;
                return parse_powershell_script(executable, script);
            }

            "-nologo" | "-noprofile" | "-noninteractive" | "-mta" | "-sta" => {
                idx += 1;
                continue;
            }

            "-encodedcommand" | "-ec" | "-file" | "/file" | "-windowstyle" | "-executionpolicy"
            | "-workingdirectory" => {
                return None;
            }

            _ if lower.starts_with('-') => {
                return None;
            }

            _ => {
                let script = join_arguments_as_script(&args[idx..]);
                return parse_powershell_script(executable, &script);
            }
        }
    }

    None
}

fn parse_powershell_script(executable: &str, script: &str) -> Option<Vec<Vec<String>>> {
    if let PowershellParseOutcome::Commands(commands) =
        parse_with_powershell_ast(executable, script)
    {
        Some(commands)
    } else {
        None
    }
}

fn is_powershell_executable(exe: &str) -> bool {
    let executable_name = Path::new(exe)
        .file_name()
        .and_then(|osstr| osstr.to_str())
        .unwrap_or(exe)
        .to_ascii_lowercase();

    matches!(
        executable_name.as_str(),
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe"
    )
}

fn join_arguments_as_script(args: &[String]) -> String {
    let mut words = Vec::with_capacity(args.len());
    if let Some((first, rest)) = args.split_first() {
        words.push(first.clone());
        for arg in rest {
            words.push(quote_argument(arg));
        }
    }
    words.join(" ")
}

fn quote_argument(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }

    if arg.chars().all(|ch| !ch.is_whitespace()) {
        return arg.to_string();
    }

    format!("'{}'", arg.replace('\'', "''"))
}

pub(crate) fn is_safe_powershell_words(words: &[String]) -> bool {
    if words.is_empty() {
        return false;
    }

    for w in words.iter() {
        let inner = w
            .trim_matches(|c| c == '(' || c == ')')
            .trim_start_matches('-')
            .to_ascii_lowercase();
        if matches!(
            inner.as_str(),
            "set-content"
                | "add-content"
                | "out-file"
                | "new-item"
                | "remove-item"
                | "move-item"
                | "copy-item"
                | "rename-item"
                | "start-process"
                | "stop-process"
        ) {
            return false;
        }
    }

    let command = words[0]
        .trim_matches(|c| c == '(' || c == ')')
        .trim_start_matches('-')
        .to_ascii_lowercase();
    match command.as_str() {
        "echo" | "write-output" | "write-host" => true,
        "dir" | "ls" | "get-childitem" | "gci" => true,
        "cat" | "type" | "gc" | "get-content" => true,
        "select-string" | "sls" | "findstr" => true,
        "measure-object" | "measure" => true,
        "get-location" | "gl" | "pwd" => true,
        "test-path" | "tp" => true,
        "resolve-path" | "rvpa" => true,
        "select-object" | "select" => true,
        "get-item" => true,

        "git" => is_safe_git_command(words),

        "rg" => is_safe_ripgrep(words),

        "set-content" | "add-content" | "out-file" | "new-item" | "remove-item" | "move-item"
        | "copy-item" | "rename-item" | "start-process" | "stop-process" => false,

        _ => false,
    }
}

fn is_safe_ripgrep(words: &[String]) -> bool {
    const UNSAFE_RIPGREP_OPTIONS_WITH_ARGS: &[&str] = &["--pre", "--hostname-bin"];
    const UNSAFE_RIPGREP_OPTIONS_WITHOUT_ARGS: &[&str] = &["--search-zip", "-z"];

    !words.iter().skip(1).any(|arg| {
        let arg_lc = arg.to_ascii_lowercase();
        UNSAFE_RIPGREP_OPTIONS_WITHOUT_ARGS.contains(&arg_lc.as_str())
            || UNSAFE_RIPGREP_OPTIONS_WITH_ARGS
                .iter()
                .any(|opt| arg_lc == *opt || arg_lc.starts_with(&format!("{opt}=")))
    })
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::powershell::try_find_pwsh_executable_blocking;
    use pretty_assertions::assert_eq;
    use std::string::ToString;

    fn vec_str(args: &[&str]) -> Vec<String> {
        args.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn recognizes_safe_powershell_wrappers() {
        assert!(is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-NoLogo",
            "-Command",
            "Get-ChildItem -Path .",
        ])));

        assert!(is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-NoProfile",
            "-Command",
            "git status",
        ])));

        assert!(is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "Get-Content",
            "Cargo.toml",
        ])));

        if let Some(pwsh) = try_find_pwsh_executable_blocking() {
            assert!(is_safe_command_windows(&[
                pwsh.as_path().to_str().unwrap().into(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "Get-ChildItem".to_string(),
            ]));
        }
    }

    #[test]
    fn accepts_full_path_powershell_invocations() {
        if !cfg!(windows) {
            return;
        }

        if let Some(pwsh) = try_find_pwsh_executable_blocking() {
            assert!(is_safe_command_windows(&[
                pwsh.as_path().to_str().unwrap().into(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "Get-ChildItem -Path .".to_string(),
            ]));
        }

        assert!(is_safe_command_windows(&vec_str(&[
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            "-Command",
            "Get-Content Cargo.toml",
        ])));
    }

    #[test]
    fn allows_read_only_pipelines_and_git_usage() {
        let Some(pwsh) = try_find_pwsh_executable_blocking() else {
            return;
        };

        let pwsh: String = pwsh.as_path().to_str().unwrap().into();
        assert!(is_safe_command_windows(&[
            pwsh.clone(),
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-Command".to_string(),
            "rg --files-with-matches foo | Measure-Object | Select-Object -ExpandProperty Count"
                .to_string()
        ]));

        assert!(is_safe_command_windows(&[
            pwsh.clone(),
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-Command".to_string(),
            "Get-Content foo.rs | Select-Object -Skip 200".to_string()
        ]));

        assert!(is_safe_command_windows(&[
            pwsh.clone(),
            "-Command".to_string(),
            "git show HEAD:foo.rs".to_string()
        ]));

        assert!(is_safe_command_windows(&[
            pwsh.clone(),
            "-Command".to_string(),
            "(Get-Content foo.rs -Raw)".to_string()
        ]));

        assert!(is_safe_command_windows(&[
            pwsh,
            "-Command".to_string(),
            "Get-Item foo.rs | Select-Object Length".to_string()
        ]));
    }

    #[test]
    fn rejects_git_global_override_options() {
        let Some(pwsh) = try_find_pwsh_executable_blocking() else {
            return;
        };

        let pwsh: String = pwsh.as_path().to_str().unwrap().into();
        for script in [
            "git -c core.pager=cat show HEAD:foo.rs",
            "git --config-env core.pager=PAGER show HEAD:foo.rs",
            "git --config-env=core.pager=PAGER show HEAD:foo.rs",
            "git --git-dir .evil-git diff HEAD~1..HEAD",
            "git --git-dir=.evil-git diff HEAD~1..HEAD",
            "git --work-tree . status",
            "git --work-tree=. status",
            "git --exec-path .git/helpers show HEAD:foo.rs",
            "git --exec-path=.git/helpers show HEAD:foo.rs",
            "git --namespace attacker show HEAD:foo.rs",
            "git --namespace=attacker show HEAD:foo.rs",
            "git --super-prefix attacker/ show HEAD:foo.rs",
            "git --super-prefix=attacker/ show HEAD:foo.rs",
        ] {
            assert!(
                !is_safe_command_windows(&[
                    pwsh.clone(),
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-Command".to_string(),
                    script.to_string(),
                ]),
                "expected {script:?} to require approval due to unsafe git global option",
            );
        }
    }

    #[test]
    fn rejects_git_subcommand_options_with_side_effects() {
        let results: Vec<(&str, bool)> = [
            "git diff --output codex_poc.txt",
            "git diff --ext-diff HEAD",
            "git log --textconv -1",
            "git show --output=codex_poc.txt HEAD",
            "git cat-file --filters HEAD:a.txt",
        ]
        .into_iter()
        .map(|script| {
            (
                script,
                is_safe_command_windows(&[
                    "powershell.exe".to_string(),
                    "-NoProfile".to_string(),
                    "-Command".to_string(),
                    script.to_string(),
                ]),
            )
        })
        .collect();

        assert_eq!(
            vec![
                ("git diff --output codex_poc.txt", false),
                ("git diff --ext-diff HEAD", false),
                ("git log --textconv -1", false),
                ("git show --output=codex_poc.txt HEAD", false),
                ("git cat-file --filters HEAD:a.txt", false),
            ],
            results
        );
    }

    #[test]
    fn rejects_stop_parsing_git_forms() {
        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-NoProfile",
            "-Command",
            "git log --% HEAD --output=codex_poc.txt",
        ])));
    }

    #[test]
    fn rejects_powershell_commands_with_side_effects() {
        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-NoLogo",
            "-Command",
            "Remove-Item foo.txt",
        ])));

        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-NoProfile",
            "-Command",
            "rg --pre cat",
        ])));

        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "Set-Content foo.txt 'hello'",
        ])));

        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "echo hi > out.txt",
        ])));
        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "Get-Content x | Out-File y",
        ])));
        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "Write-Output foo 2> err.txt",
        ])));

        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "& Remove-Item foo",
        ])));

        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "Get-ChildItem; Remove-Item foo",
        ])));
        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "Write-Output (Set-Content foo6.txt 'abc')",
        ])));
        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "Write-Host (Remove-Item foo.txt)",
        ])));
        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "Get-Content (New-Item bar.txt)",
        ])));

        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "ls @(calc.exe)"
        ])));

        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "ls && pwd"
        ])));

        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "Write-Output $(Get-Content foo)"
        ])));

        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "''"
        ])));
    }

    #[test]
    fn accepts_constant_expression_arguments() {
        assert!(is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "Get-Content 'foo bar'"
        ])));

        assert!(is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "Get-Content \"foo bar\""
        ])));
    }

    #[test]
    fn rejects_dynamic_arguments() {
        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "Get-Content $foo"
        ])));

        assert!(!is_safe_command_windows(&vec_str(&[
            "powershell.exe",
            "-Command",
            "Write-Output \"foo $bar\""
        ])));
    }

    #[test]
    fn uses_invoked_powershell_variant_for_parsing() {
        if !cfg!(windows) {
            return;
        }

        let chain = "pwd && ls";
        assert!(
            !is_safe_command_windows(&vec_str(&[
                "powershell.exe",
                "-NoProfile",
                "-Command",
                chain,
            ])),
            "`{chain}` is not recognized by powershell.exe"
        );

        if let Some(pwsh) = try_find_pwsh_executable_blocking() {
            assert!(
                is_safe_command_windows(&[
                    pwsh.as_path().to_str().unwrap().into(),
                    "-NoProfile".to_string(),
                    "-Command".to_string(),
                    chain.to_string(),
                ]),
                "`{chain}` should be considered safe to pwsh.exe"
            );
        }
    }
}
