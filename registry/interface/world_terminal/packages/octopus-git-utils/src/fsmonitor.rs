use std::future::Future;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FsmonitorOverride {
    Disabled,
    BuiltIn,
}

impl FsmonitorOverride {
    pub const fn git_config_arg(self) -> &'static str {
        match self {
            Self::Disabled => "core.fsmonitor=false",
            Self::BuiltIn => "core.fsmonitor=true",
        }
    }
}

pub trait FsmonitorProbeRunner: Send {
    fn run_probe(&mut self, args: &[&str]) -> impl Future<Output = Option<Vec<u8>>> + Send;
}

pub async fn detect_fsmonitor_override(
    runner: &mut impl FsmonitorProbeRunner,
) -> FsmonitorOverride {
    let Some(config) = runner
        .run_probe(&["config", "--null", "--get", "core.fsmonitor"])
        .await
    else {
        return FsmonitorOverride::Disabled;
    };
    let Some(config) = config.strip_suffix(b"\0") else {
        return FsmonitorOverride::Disabled;
    };
    if config.contains(&0) {
        return FsmonitorOverride::Disabled;
    }
    let Ok(config) = str::from_utf8(config) else {
        return FsmonitorOverride::Disabled;
    };

    let configured = if ["true", "yes", "on"]
        .iter()
        .any(|value| config.eq_ignore_ascii_case(value))
    {
        true
    } else if ["false", "no", "off"]
        .iter()
        .any(|value| config.eq_ignore_ascii_case(value))
    {
        false
    } else {
        let typed_args = [
            "config",
            "--null",
            "--type=bool",
            "--fixed-value",
            "--get",
            "core.fsmonitor",
            config,
        ];
        matches!(
            runner.run_probe(&typed_args).await.as_deref(),
            Some(b"true\0")
        )
    };
    if !configured {
        return FsmonitorOverride::Disabled;
    }

    let Some(build_options) = runner.run_probe(&["version", "--build-options"]).await else {
        return FsmonitorOverride::Disabled;
    };
    if build_options
        .split(|byte| *byte == b'\n')
        .any(|line| line.trim_ascii() == b"feature: fsmonitor--daemon")
    {
        FsmonitorOverride::BuiltIn
    } else {
        FsmonitorOverride::Disabled
    }
}

#[cfg(test)]
#[path = "fsmonitor_tests.rs"]
mod tests;
