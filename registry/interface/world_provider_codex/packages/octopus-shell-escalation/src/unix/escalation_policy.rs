use std::future::Future;
use std::pin::Pin;

use octopus_util_abspath::AbsolutePathBuf;

use crate::unix::escalate_protocol::EscalationDecision;

pub trait EscalationPolicy: Send + Sync {
    fn determine_action<'a>(
        &'a self,
        file: &'a AbsolutePathBuf,
        argv: &'a [String],
        workdir: &'a AbsolutePathBuf,
    ) -> EscalationPolicyFuture<'a>;
}

pub type EscalationPolicyFuture<'a> =
    Pin<Box<dyn Future<Output = anyhow::Result<EscalationDecision>> + Send + 'a>>;
