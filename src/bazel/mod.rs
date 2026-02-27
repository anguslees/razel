pub(crate) mod bzlmod;
pub(crate) mod label;
pub(crate) mod package;
pub(crate) mod repo;

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct Configuration {
    pub ignore_dev_dependency: bool,
}

impl Configuration {
    pub(crate) fn from_flags(cli: &crate::Cli) -> Self {
        Self {
            ignore_dev_dependency: cli.ignore_dev_dependency,
        }
    }
}
