use crate::bazel::label::{CanonicalRepo, MAIN_REPO};
use crate::bazel::package::{BoxFileStore, DynFileStore};
use crate::bazel::repo::{LocalFileStore, Repository};
use crate::shared_error::SharedError;
use futures::TryFutureExt;
use futures::future::{BoxFuture, Shared};
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

type RepositoryFuture = Shared<BoxFuture<'static, Result<Arc<Repository<'static>>, SharedError>>>;

/// The environment shared by all Bazel commands run in the same main repository. It encompasses the main repo and the set of all defined external repos.
///
/// Most interactions with this codebase start from the Workspace.
pub struct Workspace {
    path: PathBuf,
    repositories: RwLock<HashMap<CanonicalRepo<'static>, RepositoryFuture>>,
}

async fn any_exists(file1: impl AsRef<Path>, file2: impl AsRef<Path>) -> std::io::Result<bool> {
    let mut tasks = FuturesUnordered::new();
    tasks.push(tokio::fs::try_exists(file1.as_ref()));
    tasks.push(tokio::fs::try_exists(file2.as_ref()));

    while let Some(res) = tasks.next().await {
        match res {
            Ok(true) => return Ok(true),
            Ok(false) => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}

impl Workspace {
    pub async fn new(start_dir: impl AsRef<Path>) -> Result<Arc<Self>, std::io::Error> {
        let mut current_dir = std::path::absolute(start_dir)?;

        loop {
            if any_exists(
                current_dir.join("MODULE.bazel"),
                current_dir.join("REPO.bazel"),
            )
            .await?
            {
                break;
            }

            if !current_dir.pop() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not find MODULE.bazel or REPO.bazel in current or any parent directory",
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

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[allow(dead_code)]
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

    #[allow(dead_code)]
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
