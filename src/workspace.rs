use futures::future::{BoxFuture, Shared};
use futures::{FutureExt, TryFutureExt};
use std::future::IntoFuture;

use crate::bazel::label::{CanonicalRepo, MAIN_REPO};
use crate::bazel::package::{BoxFileStore, DynFileStore};
use crate::bazel::repo::{LocalFileStore, Repository};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::shared_error::SharedError;

type RepositoryFuture = Shared<BoxFuture<'static, Result<Arc<Repository<'static>>, SharedError>>>;

/// The environment shared by all Bazel commands run in the same main repository. It encompasses the main repo and the set of all defined external repos.
///
/// Most interactions with this codebase start from the Workspace.
pub struct Workspace {
    pub path: PathBuf,
    repositories: RwLock<HashMap<CanonicalRepo<'static>, RepositoryFuture>>,
}

impl Workspace {
    // TODO: this should be async
    pub fn new<P: AsRef<Path>>(start_dir: P) -> Result<Arc<Self>, std::io::Error> {
        let mut current_dir = start_dir.as_ref().to_path_buf();

        loop {
            let module_bazel = current_dir.join("MODULE.bazel");
            let repo_bazel = current_dir.join("REPO.bazel");

            if module_bazel.exists() || repo_bazel.exists() {
                break;
            }

            if !current_dir.pop() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not find MODULE.bazel or REPO.bazel in parent directories",
                ));
            }
        }

        let ws = Arc::new(Workspace {
            path: current_dir.clone(),
            repositories: RwLock::new(HashMap::new()),
        });

        // Create the main repository
        // Use Box to implement FileStore for BoxedFileStore
        let files: BoxFileStore<'static> = std::sync::Arc::from(DynFileStore::new_box(Box::new(
            crate::bazel::package::TypeErasingFileStore(LocalFileStore::new(current_dir)),
        )));

        ws.add_repository(MAIN_REPO, Repository::new(ws.clone(), MAIN_REPO, files));

        Ok(ws)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn main_repo(&self) -> anyhow::Result<Arc<Repository<'static>>> {
        let repo_future = self
            .repositories
            .read()
            .unwrap()
            .get(&MAIN_REPO)
            .expect("Main repo not found")
            .clone();
        // Avoid holding the lock while awaiting
        repo_future
            .await
            .map_err(|e| anyhow::Error::new(e).context("Failed to evaluate main repo"))
    }

    pub async fn main_module(&self) -> anyhow::Result<crate::bazel::bzlmod::Module> {
        let repo = self.main_repo().await?;
        crate::bazel::bzlmod::eval_module(repo.files(), "MODULE.bazel", true).await
    }

    pub fn add_repository<Fut>(&self, repo: CanonicalRepo<'static>, f: Fut)
    where
        Fut: IntoFuture<Output = Result<Repository<'static>, anyhow::Error>>,
        Fut::IntoFuture: Send + 'static,
    {
        let f = f.into_future().map_ok(Arc::new).map_err(SharedError::from);
        self.repositories
            .write()
            .unwrap()
            .insert(repo, f.boxed().shared());
    }
}
