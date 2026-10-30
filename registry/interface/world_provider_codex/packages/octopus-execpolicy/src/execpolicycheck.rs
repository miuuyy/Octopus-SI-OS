use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use serde::Serialize;

use crate::Decision;
use crate::MatchOptions;
use crate::Policy;
use crate::PolicyParser;
use crate::RuleMatch;

#[derive(Debug, Parser, Clone)]
pub struct ExecPolicyCheckCommand {
    #[arg(short = 'r', long = "rules", value_name = "PATH", required = true)]
    pub rules: Vec<PathBuf>,

    #[arg(long)]
    pub pretty: bool,

    #[arg(long)]
    pub resolve_host_executables: bool,

    #[arg(
        value_name = "COMMAND",
        required = true,
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub command: Vec<String>,
}

impl ExecPolicyCheckCommand {
    pub fn run(&self) -> Result<()> {
        let policy = load_policies(&self.rules)?;
        let matched_rules = policy.matches_for_command_with_options(
            &self.command,
            None,
            &MatchOptions {
                resolve_host_executables: self.resolve_host_executables,
            },
        );

        let json = format_matches_json(&matched_rules, self.pretty)?;
        println!("{json}");

        Ok(())
    }
}

pub fn format_matches_json(matched_rules: &[RuleMatch], pretty: bool) -> Result<String> {
    let output = ExecPolicyCheckOutput {
        matched_rules,
        decision: matched_rules.iter().map(RuleMatch::decision).max(),
    };

    if pretty {
        serde_json::to_string_pretty(&output).map_err(Into::into)
    } else {
        serde_json::to_string(&output).map_err(Into::into)
    }
}

pub fn load_policies(policy_paths: &[PathBuf]) -> Result<Policy> {
    let mut parser = PolicyParser::new();

    for policy_path in policy_paths {
        let policy_file_contents = fs::read_to_string(policy_path)
            .with_context(|| format!("failed to read policy at {}", policy_path.display()))?;
        let policy_identifier = policy_path.to_string_lossy().to_string();
        parser
            .parse(&policy_identifier, &policy_file_contents)
            .with_context(|| format!("failed to parse policy at {}", policy_path.display()))?;
    }

    Ok(parser.build())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecPolicyCheckOutput<'a> {
    #[serde(rename = "matchedRules")]
    matched_rules: &'a [RuleMatch],
    #[serde(skip_serializing_if = "Option::is_none")]
    decision: Option<Decision>,
}
