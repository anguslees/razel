pub(crate) mod bzlmod;
pub(crate) mod repo;

#[derive(Debug)]
pub(crate) struct Configuration {
    pub ignore_dev_dependency: bool,
}

impl Configuration {
    pub(crate) fn new() -> Self {
        Self {
            ignore_dev_dependency: false,
        }
    }
}