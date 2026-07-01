use crate::bazel::label::{CanonicalRepo, Label, MAIN_REPO, Repo, TargetPattern};
use crate::bazel::package::{BoxFileStore, DynFileStore};
use crate::bazel::repo::{LocalFileStore, Repository};
use crate::shared_error::SharedError;
use futures::TryFutureExt;
use futures::future::{BoxFuture, Shared};
use futures::stream::{FuturesUnordered, Stream};
use futures::{FutureExt, StreamExt};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use starlark::environment::FrozenModule;

type RepositoryFuture = Shared<BoxFuture<'static, Result<Arc<Repository<'static>>, SharedError>>>;
type FrozenModuleFuture = Shared<BoxFuture<'static, Result<FrozenModule, SharedError>>>;

/// The environment shared by all Bazel commands run in the same main repository. It encompasses the main repo and the set of all defined external repos.
///
/// Most interactions with this codebase start from the Workspace.
pub struct Workspace {
    path: PathBuf,
    repositories: RwLock<HashMap<CanonicalRepo<'static>, RepositoryFuture>>,
    loaded_deps: RwLock<HashMap<crate::bazel::label::CanonicalLabel<'static>, FrozenModuleFuture>>,
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
            loaded_deps: RwLock::new(HashMap::new()),
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

    pub fn get_or_add_bzl<Fut>(
        &self,
        label: crate::bazel::label::CanonicalLabel<'static>,
        f: impl FnOnce() -> Fut,
    ) -> FrozenModuleFuture
    where
        Fut: IntoFuture<Output = Result<FrozenModule, anyhow::Error>>,
        Fut::IntoFuture: Send + 'static,
    {
        // First, check with read lock
        if let Some(future) = self.loaded_deps.read().unwrap().get(&label) {
            return future.clone();
        }

        // Retry with write lock and insert if still absent
        let mut deps = self.loaded_deps.write().unwrap();
        if let Some(future) = deps.get(&label) {
            return future.clone();
        }

        let future = f()
            .into_future()
            .map_err(SharedError::from)
            .boxed()
            .shared();
        deps.insert(label, future.clone());
        future
    }

    /// Expand pattern into a stream of Labels
    pub fn expand_pattern<'a>(
        self: &Arc<Self>,
        pattern: TargetPattern<'a>,
    ) -> impl Stream<Item = anyhow::Result<Label<'a>>> + 'a {
        let ws = self.clone();

        let fut = async move {
            let repo = match ws.main_repo().await {
                Ok(r) => r,
                Err(e) => return futures::stream::once(async move { Err(e) }).boxed(),
            };

            let mut packages = Vec::new();
            let base_dir = ws.path.clone();
            if let Err(e) = walk_packages(base_dir.clone(), base_dir.clone(), &mut packages).await {
                return futures::stream::once(async move { Err(anyhow::Error::from(e)) }).boxed();
            }

            let packages_stream = futures::stream::iter(packages);
            
            let labels_stream = packages_stream.then(move |pkg| {
                let ws_clone = ws.clone();
                let repo_clone = repo.clone();
                let pattern_clone = pattern.clone();
                let base_dir_clone = base_dir.clone();
                
                async move {
                    let build_path = if pkg.is_empty() {
                        if tokio::fs::try_exists(base_dir_clone.join("BUILD.bazel")).await.unwrap_or(false) {
                            "BUILD.bazel".to_string()
                        } else {
                            "BUILD".to_string()
                        }
                    } else {
                        if tokio::fs::try_exists(base_dir_clone.join(&pkg).join("BUILD.bazel")).await.unwrap_or(false) {
                            format!("{}/BUILD.bazel", pkg)
                        } else {
                            format!("{}/BUILD", pkg)
                        }
                    };

                    match crate::starlark::eval::eval_build(ws_clone, repo_clone.clone(), &build_path).await {
                        Ok(rules) => {
                            let mut matched = Vec::new();
                            for rule_name in rules.keys() {
                                let static_label = Label::new(
                                    Repo::Canonical(repo_clone.canonical_name()),
                                    pkg.clone(),
                                    rule_name.clone(),
                                );
                                // Convert to 'a lifetime
                                let label_a = {
                                    let repo_a = match static_label.repo {
                                        Repo::Apparent(r) => Repo::Apparent(crate::bazel::label::ApparentRepo::new(r.into_name())),
                                        Repo::Canonical(r) => Repo::Canonical(crate::bazel::label::CanonicalRepo::new(r.into_name())),
                                    };
                                    Label::new(
                                        repo_a,
                                        static_label.package.into_owned(),
                                        static_label.target.into_owned(),
                                    )
                                };

                                if pattern_clone.matches(&label_a) {
                                    matched.push(Ok(label_a));
                                }
                            }
                            futures::stream::iter(matched).boxed()
                        }
                        Err(e) => futures::stream::once(async move { Err(e) }).boxed(),
                    }
                }
            }).flatten();
            
            labels_stream.boxed()
        };

        futures::stream::once(fut).flatten()
    }
}

#[async_recursion::async_recursion]
async fn walk_packages(
    dir: PathBuf,
    base_dir: PathBuf,
    packages: &mut Vec<String>,
) -> anyhow::Result<()> {
    let mut read_dir = tokio::fs::read_dir(&dir).await?;
    let mut has_build = false;
    let mut subdirs = Vec::new();

    while let Some(entry) = read_dir.next_entry().await? {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || name_str == "target" || name_str.starts_with("bazel-") {
            continue;
        }

        let file_type = entry.file_type().await?;
        if file_type.is_dir() {
            subdirs.push(entry.path());
        } else if file_type.is_file() {
            if name_str == "BUILD" || name_str == "BUILD.bazel" {
                has_build = true;
            }
        }
    }

    if has_build {
        let rel_path = dir.strip_prefix(&base_dir)?;
        packages.push(rel_path.to_string_lossy().into_owned());
    }

    for subdir in subdirs {
        walk_packages(subdir, base_dir.clone(), packages).await?;
    }

    Ok(())
}
