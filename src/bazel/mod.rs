pub(crate) mod bzlmod;
pub(crate) mod label;
pub(crate) mod package;
pub(crate) mod repo;
pub(crate) mod rule;
pub(crate) mod target;

#[derive(Debug, Clone)]
pub(crate) struct InvocationOptions {
    pub ignore_dev_dependency: bool,
}

impl InvocationOptions {
    pub(crate) fn from_flags(cli: &crate::Cli) -> Self {
        Self {
            ignore_dev_dependency: cli.ignore_dev_dependency,
        }
    }
}
