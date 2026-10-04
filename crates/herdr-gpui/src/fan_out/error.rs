//! Fan-out failures keep the step they happened in and their typed cause.

/// The part of a fan-out that failed, for context in diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Detect,
    CreateWorktree,
    ResolveBase,
    StartAgent,
    Prompt,
    Compare,
    Remove,
}

impl Step {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Detect => "finding installed agents",
            Self::CreateWorktree => "creating the worktree",
            Self::ResolveBase => "reading the base commit",
            Self::StartAgent => "starting the agent",
            Self::Prompt => "sending the prompt",
            Self::Compare => "reading the changes",
            Self::Remove => "removing the worktree",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("{} failed: {source}", step.label())]
    Script {
        step: Step,
        #[source]
        source: herdr_client::Error,
    },
    #[error("{} failed: unexpected output", step.label())]
    Decode {
        step: Step,
        #[source]
        source: serde_json::Error,
    },
    #[error("The new checkout reported no commit to compare against")]
    NoBase,
    #[error("Fan-out cancelled")]
    Cancelled,
}

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

/// `map_err` adapter attaching the step to a script failure.
pub(crate) fn script(step: Step) -> impl FnOnce(herdr_client::Error) -> Error {
    move |source| match source {
        herdr_client::Error::ScriptCancelled => Error::Cancelled,
        source => Error::Script { step, source },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_failures_keep_step_and_source() {
        let error = script(Step::StartAgent)(herdr_client::Error::ScriptTimeout);
        assert!(matches!(
            error,
            Error::Script {
                step: Step::StartAgent,
                source: herdr_client::Error::ScriptTimeout
            }
        ));
        assert!(std::error::Error::source(&error).is_some());
        assert_eq!(
            error.to_string(),
            "starting the agent failed: host script made no progress before its deadline"
        );
        assert!(matches!(
            script(Step::Prompt)(herdr_client::Error::ScriptCancelled),
            Error::Cancelled
        ));
    }
}
