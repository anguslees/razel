use std::path::{Path, PathBuf};

pub struct Workspace {
    pub path: PathBuf,
}

impl Workspace {
    pub fn new<P: AsRef<Path>>(start_dir: P) -> Result<Self, std::io::Error> {
        let mut current_dir = start_dir.as_ref().to_path_buf();

        loop {
            let module_bazel = current_dir.join("MODULE.bazel");
            let repo_bazel = current_dir.join("REPO.bazel");

            if module_bazel.exists() || repo_bazel.exists() {
                return Ok(Workspace { path: current_dir });
            }

            if !current_dir.pop() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not find MODULE.bazel or REPO.bazel in parent directories",
                ));
            }
        }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}
