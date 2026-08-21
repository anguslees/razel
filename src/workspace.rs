#![allow(dead_code)]

use crate::bazel::InvocationOptions;
use crate::bazel::label::{
    CanonicalLabel, CanonicalRepo, MAIN_REPO, Repo, TargetPattern, TargetPatternKind,
};
use crate::bazel::package::{BoxFileStore, DynFileStore, Package, PackageId};
use crate::bazel::repo::{LocalFileStore, PackageNotFound, Repository};
use crate::bazel::target::{Target, TargetView};
use crate::shared_error::SharedError;
use futures::FutureExt;
use futures::TryFutureExt;
use futures::future::{BoxFuture, Shared};
use futures::stream::{BoxStream, FuturesUnordered, StreamExt};
use starlark::environment::FrozenModule;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, Weak};

type RepositoryFuture = Shared<BoxFuture<'static, Result<Arc<Repository<'static>>, SharedError>>>;
type FrozenModuleFuture = Shared<BoxFuture<'static, Result<FrozenModule, SharedError>>>;
type LoadedPackageFuture = Shared<BoxFuture<'static, Result<Arc<Package>, SharedError>>>;

enum LoadedPackageEntry {
    Loading(LoadedPackageFuture),
    Loaded(Weak<Package>),
    Failed(SharedError),
}

pub struct Workspace {
    path: PathBuf,
    invocation_options: Arc<InvocationOptions>,
    repositories: RwLock<HashMap<CanonicalRepo<'static>, RepositoryFuture>>,
    loaded_deps: RwLock<HashMap<CanonicalLabel<'static>, FrozenModuleFuture>>,
    loaded_packages: RwLock<HashMap<PackageId<'static>, LoadedPackageEntry>>,
}

#[derive(Debug, Clone)]
pub enum ExpandedTargetKind {
    Rule(String),
    SourceFile,
    GeneratedFile,
}

#[derive(Debug, Clone)]
pub struct ExpandedTarget {
    pub label: CanonicalLabel<'static>,
    pub kind: ExpandedTargetKind,
}

async fn any_exists(file1: impl AsRef<Path>, file2: impl AsRef<Path>) -> std::io::Result<bool> {
    let mut tasks = FuturesUnordered::new();
    tasks.push(tokio::fs::try_exists(file1.as_ref()));
    tasks.push(tokio::fs::try_exists(file2.as_ref()));
    while let Some(result) = tasks.next().await {
        match result {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

fn package_entry_future(entry: &LoadedPackageEntry) -> Option<LoadedPackageFuture> {
    match entry {
        LoadedPackageEntry::Loading(future) => Some(future.clone()),
        LoadedPackageEntry::Loaded(package) => package
            .upgrade()
            .map(|package| futures::future::ready(Ok(package)).boxed().shared()),
        LoadedPackageEntry::Failed(error) => {
            Some(futures::future::ready(Err(error.clone())).boxed().shared())
        }
    }
}

impl Workspace {
    pub async fn new(
        start_dir: impl AsRef<Path>,
        invocation_options: Arc<InvocationOptions>,
    ) -> Result<Arc<Self>, std::io::Error> {
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

        let workspace = Arc::new(Self {
            path: current_dir.clone(),
            invocation_options,
            repositories: RwLock::new(HashMap::new()),
            loaded_deps: RwLock::new(HashMap::new()),
            loaded_packages: RwLock::new(HashMap::new()),
        });
        let files: BoxFileStore<'static> = Arc::from(DynFileStore::new_box(Box::new(
            crate::bazel::package::TypeErasingFileStore(LocalFileStore::new(current_dir)),
        )));
        workspace.add_repository(
            MAIN_REPO,
            Repository::new(workspace.clone(), MAIN_REPO, files),
        );
        Ok(workspace)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn invocation_options(&self) -> &InvocationOptions {
        &self.invocation_options
    }

    pub async fn main_repo(&self) -> anyhow::Result<Arc<Repository<'static>>> {
        self.repository(&MAIN_REPO).await
    }

    async fn repository<'a>(
        &self,
        name: &CanonicalRepo<'a>,
    ) -> anyhow::Result<Arc<Repository<'static>>> {
        let future = self
            .repositories
            .read()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Repository `{name}` is not defined"))?;
        future
            .await
            .map_err(|error| anyhow::Error::new(error).context("Failed to evaluate repository"))
    }

    pub async fn main_module(&self) -> anyhow::Result<crate::bazel::bzlmod::Module> {
        let repo = self.main_repo().await?;
        crate::bazel::bzlmod::eval_module(
            repo.files(),
            "MODULE.bazel",
            true,
            self.invocation_options.ignore_dev_dependency,
        )
        .await
    }

    pub fn add_repository<Fut>(&self, repo: CanonicalRepo<'static>, future: Fut)
    where
        Fut: IntoFuture<Output = Result<Repository<'static>, anyhow::Error>>,
        Fut::IntoFuture: Send + 'static,
    {
        let future = future
            .into_future()
            .map_ok(Arc::new)
            .map_err(SharedError::from);
        self.repositories
            .write()
            .unwrap()
            .insert(repo, future.boxed().shared());
    }

    pub fn get_or_add_bzl<Fut>(
        &self,
        label: CanonicalLabel<'static>,
        load: impl FnOnce() -> Fut,
    ) -> FrozenModuleFuture
    where
        Fut: IntoFuture<Output = Result<FrozenModule, anyhow::Error>>,
        Fut::IntoFuture: Send + 'static,
    {
        if let Some(future) = self.loaded_deps.read().unwrap().get(&label) {
            return future.clone();
        }
        let mut loaded_deps = self.loaded_deps.write().unwrap();
        if let Some(future) = loaded_deps.get(&label) {
            return future.clone();
        }
        let future = load()
            .into_future()
            .map_err(SharedError::from)
            .boxed()
            .shared();
        loaded_deps.insert(label, future.clone());
        future
    }

    fn load_package(self: &Arc<Self>, id: PackageId<'static>) -> LoadedPackageFuture {
        let workspace = self.clone();
        self.get_or_add_package(id, move |load_id| async move {
            let repository = workspace.repository(&load_id.repository).await?;
            let source = repository.read_package(&load_id.package).await?;
            repository
                .eval_package(&source, load_id, workspace.clone())
                .await
                .map(Arc::new)
        })
    }

    fn get_or_add_package<Fut>(
        self: &Arc<Self>,
        id: PackageId<'static>,
        load: impl FnOnce(PackageId<'static>) -> Fut + Send + 'static,
    ) -> LoadedPackageFuture
    where
        Fut: IntoFuture<Output = Result<Arc<Package>, anyhow::Error>>,
        Fut::IntoFuture: Send + 'static,
    {
        if let Some(future) = self
            .loaded_packages
            .read()
            .unwrap()
            .get(&id)
            .and_then(package_entry_future)
        {
            return future;
        }
        let mut loaded_packages = self.loaded_packages.write().unwrap();
        if let Some(future) = loaded_packages.get(&id).and_then(package_entry_future) {
            return future;
        }
        let load_id = id.clone();
        let cache_id = id.clone();
        let workspace = Arc::downgrade(self);
        let future = async move {
            let result = load(load_id).into_future().await.map_err(SharedError::from);
            if let Some(workspace) = workspace.upgrade() {
                let entry = match &result {
                    Ok(package) => LoadedPackageEntry::Loaded(Arc::downgrade(package)),
                    Err(error) => LoadedPackageEntry::Failed(error.clone()),
                };
                workspace
                    .loaded_packages
                    .write()
                    .unwrap()
                    .insert(cache_id, entry);
            }
            result
        }
        .boxed()
        .shared();
        loaded_packages.insert(id, LoadedPackageEntry::Loading(future.clone()));
        future
    }

    pub fn expand_pattern<'a>(
        self: Arc<Self>,
        pattern: TargetPattern<'a>,
    ) -> BoxStream<'a, anyhow::Result<ExpandedTarget>> {
        Box::pin(async_stream::try_stream! {
            let repository = match &pattern.repo {
                Repo::Canonical(repository) => self.repository(repository).await?,
                Repo::Apparent(apparent) => {
                    let main_repository = self.main_repo().await?;
                    let canonical = main_repository
                        .resolve_repo(apparent)
                        .ok_or_else(|| anyhow::anyhow!("Unknown apparent repository `{apparent}`"))?;
                    self.repository(&canonical).await?
                }
            };
            let package_path = pattern.package;
            let selection = match pattern.target_kind {
                TargetPatternKind::Exact(name) => PatternSelection::Exact(name),
                TargetPatternKind::AllRules => PatternSelection::AllRules,
                TargetPatternKind::AllTargets => PatternSelection::AllTargets,
            };
            let include_subpackages = pattern.include_subpackages;
            let root_id = PackageId::new(
                repository.canonical_name().clone(),
                package_path.clone().into_owned(),
            );
            match self.load_package(root_id).await {
                Ok(package) => {
                    for target in select_targets(&package, &selection) {
                        yield target;
                    }
                }
                Err(error) if include_subpackages && is_not_found(&error) => {}
                Err(error) => Err(anyhow::Error::new(error))?,
            }

            if include_subpackages {
                let mut subpackages = repository.subpackage_paths(package_path.as_ref());
                while let Some(package_path) = subpackages.next().await {
                    let package = self
                        .load_package(PackageId::new(
                            repository.canonical_name().clone(),
                            package_path?,
                        ))
                        .await
                        .map_err(anyhow::Error::new)?;
                    for target in select_targets(&package, &selection) {
                        yield target;
                    }
                }
            }
        })
    }
}

fn is_not_found(error: &SharedError) -> bool {
    error.0.chain().any(|cause| {
        cause.downcast_ref::<PackageNotFound>().is_some()
            || cause
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::get_ref)
                .and_then(|inner| inner.downcast_ref::<PackageNotFound>())
                .is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bazel::package::PackageBuilder;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_workspace() -> Arc<Workspace> {
        Arc::new(Workspace {
            path: PathBuf::new(),
            invocation_options: Arc::new(InvocationOptions {
                ignore_dev_dependency: false,
            }),
            repositories: RwLock::new(HashMap::new()),
            loaded_deps: RwLock::new(HashMap::new()),
            loaded_packages: RwLock::new(HashMap::new()),
        })
    }

    fn empty_package(id: PackageId<'static>) -> Arc<Package> {
        Arc::new(
            PackageBuilder::new(id, "BUILD.bazel")
                .unwrap()
                .finish(&HashMap::new())
                .unwrap(),
        )
    }

    #[tokio::test]
    async fn package_cache_shares_success_and_failure() {
        let workspace = test_workspace();
        let id = PackageId::new(MAIN_REPO, "cached");
        let loads = Arc::new(AtomicUsize::new(0));
        let first_loads = loads.clone();
        let first_id = id.clone();
        let first = workspace.get_or_add_package(id.clone(), move |_| async move {
            first_loads.fetch_add(1, Ordering::SeqCst);
            Ok(empty_package(first_id))
        });
        let second_loads = loads.clone();
        let second_id = id.clone();
        let second = workspace.get_or_add_package(id, move |_| async move {
            second_loads.fetch_add(1, Ordering::SeqCst);
            Ok(empty_package(second_id))
        });
        let (first, second) = futures::join!(first, second);
        let first = first.unwrap();
        let second = second.unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        drop(first);
        drop(second);

        let reloads = loads.clone();
        let reload_id = PackageId::new(MAIN_REPO, "cached");
        workspace
            .get_or_add_package(reload_id.clone(), move |_| async move {
                reloads.fetch_add(1, Ordering::SeqCst);
                Ok(empty_package(reload_id))
            })
            .await
            .unwrap();
        assert_eq!(loads.load(Ordering::SeqCst), 2);

        let failed_id = PackageId::new(MAIN_REPO, "failed");
        let failures = Arc::new(AtomicUsize::new(0));
        let first_failures = failures.clone();
        assert!(
            workspace
                .get_or_add_package(failed_id.clone(), move |_| async move {
                    first_failures.fetch_add(1, Ordering::SeqCst);
                    anyhow::bail!("cached failure")
                })
                .await
                .is_err()
        );
        let second_failures = failures.clone();
        assert!(
            workspace
                .get_or_add_package(failed_id, move |_| async move {
                    second_failures.fetch_add(1, Ordering::SeqCst);
                    anyhow::bail!("unexpected reload")
                })
                .await
                .is_err()
        );
        assert_eq!(failures.load(Ordering::SeqCst), 1);
    }
}

#[derive(Debug)]
enum PatternSelection<'a> {
    Exact(Cow<'a, str>),
    AllRules,
    AllTargets,
}

fn select_targets<'a>(
    package: &'a Package,
    selection: &'a PatternSelection<'_>,
) -> impl Iterator<Item = ExpandedTarget> + 'a {
    package
        .targets()
        .filter(move |target| match selection {
            PatternSelection::Exact(name) => target.name() == name,
            PatternSelection::AllRules => matches!(target.target(), Target::Rule(_)),
            PatternSelection::AllTargets => true,
        })
        .map(expand_target)
}

fn expand_target(target: TargetView<'_>) -> ExpandedTarget {
    let label = target.label().clone().into_owned();
    let kind = match target.target() {
        Target::Rule(rule) => ExpandedTargetKind::Rule(rule.rule_class().display_name().to_owned()),
        Target::SourceFile(_) => ExpandedTargetKind::SourceFile,
        Target::GeneratedFile(_) => ExpandedTargetKind::GeneratedFile,
    };
    ExpandedTarget { label, kind }
}
