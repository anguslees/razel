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
use std::sync::{Arc, Mutex, RwLock, Weak};

type RepositoryFuture = Shared<BoxFuture<'static, Result<Arc<Repository<'static>>, SharedError>>>;
type FrozenModuleFuture = Shared<BoxFuture<'static, Result<FrozenModule, SharedError>>>;
type LoadedPackageFuture = Shared<BoxFuture<'static, Result<Arc<Package>, SharedError>>>;

/// How many packages each generation of [`RecentPackages`] keeps loaded.
const RECENT_PACKAGES: usize = 512;

enum LoadedPackageEntry {
    Loading(LoadedPackageFuture),
    Loaded(Weak<Package>),
    Failed(SharedError),
}

/// Keeps recently used packages loaded, so packages that many targets depend on, such as common
/// libraries, are not evaluated again each time they are needed.
///
/// This approximates LRU with two generations of `capacity` packages: a package stays loaded while
/// it is used at least once per generation, so between `capacity` and twice that many packages
/// are kept. Independently of this, the weak `loaded_packages` map always reuses a package that is
/// still referenced elsewhere.
struct RecentPackages {
    capacity: usize,
    current: HashMap<PackageId<'static>, Arc<Package>>,
    previous: HashMap<PackageId<'static>, Arc<Package>>,
}

impl RecentPackages {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            current: HashMap::new(),
            previous: HashMap::new(),
        }
    }

    fn touch(&mut self, id: &PackageId<'static>, package: &Arc<Package>) {
        if self.capacity == 0 || self.current.contains_key(id) {
            return;
        }
        self.previous.remove(id);
        if self.current.len() >= self.capacity {
            // Start a new generation, releasing packages unused during the last one.
            self.previous = std::mem::take(&mut self.current);
        }
        self.current.insert(id.clone(), package.clone());
    }
}

pub struct Workspace {
    path: PathBuf,
    invocation_options: Arc<InvocationOptions>,
    repositories: RwLock<HashMap<CanonicalRepo<'static>, RepositoryFuture>>,
    loaded_deps: RwLock<HashMap<CanonicalLabel<'static>, FrozenModuleFuture>>,
    loaded_packages: RwLock<HashMap<PackageId<'static>, LoadedPackageEntry>>,
    recent_packages: Mutex<RecentPackages>,
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

#[derive(Debug, Clone)]
pub struct LoadedTarget {
    pub expanded: ExpandedTarget,
    pub target: Target,
    // Keep same-package lookups cached while the caller processes this target.
    _package: Arc<Package>,
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

impl Workspace {
    pub async fn find_root(start_dir: impl AsRef<Path>) -> std::io::Result<Option<PathBuf>> {
        let mut current_dir = std::path::absolute(start_dir)?;
        loop {
            if any_exists(
                current_dir.join("MODULE.bazel"),
                current_dir.join("REPO.bazel"),
            )
            .await?
            {
                return Ok(Some(current_dir));
            }
            if !current_dir.pop() {
                return Ok(None);
            }
        }
    }

    pub async fn new(
        start_dir: impl AsRef<Path>,
        invocation_options: Arc<InvocationOptions>,
    ) -> Result<Arc<Self>, std::io::Error> {
        let current_dir = Self::find_root(start_dir).await?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not find MODULE.bazel or REPO.bazel in current or any parent directory",
            )
        })?;

        let workspace = Arc::new(Self {
            path: current_dir.clone(),
            invocation_options,
            repositories: RwLock::new(HashMap::new()),
            loaded_deps: RwLock::new(HashMap::new()),
            loaded_packages: RwLock::new(HashMap::new()),
            recent_packages: Mutex::new(RecentPackages::new(RECENT_PACKAGES)),
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

    pub async fn load_target(
        self: &Arc<Self>,
        label: &CanonicalLabel<'_>,
    ) -> anyhow::Result<Option<LoadedTarget>> {
        let package = self
            .load_package(PackageId::new(
                label.repo.clone().into_owned(),
                label.package().to_owned(),
            ))
            .await
            .map_err(anyhow::Error::new)?;
        let Some(target) = package.target(label.name()) else {
            return Ok(None);
        };
        Ok(Some(LoadedTarget {
            expanded: expand_target(target.clone()),
            target: target.target().clone(),
            _package: package,
        }))
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
            .and_then(|entry| self.cached_package(&id, entry))
        {
            return future;
        }
        let mut loaded_packages = self.loaded_packages.write().unwrap();
        if let Some(future) = loaded_packages
            .get(&id)
            .and_then(|entry| self.cached_package(&id, entry))
        {
            return future;
        }
        let load_id = id.clone();
        let cache_id = id.clone();
        let workspace = Arc::downgrade(self);
        let future = async move {
            let result = load(load_id).into_future().await.map_err(SharedError::from);
            if let Some(workspace) = workspace.upgrade() {
                let entry = match &result {
                    Ok(package) => {
                        workspace
                            .recent_packages
                            .lock()
                            .unwrap()
                            .touch(&cache_id, package);
                        LoadedPackageEntry::Loaded(Arc::downgrade(package))
                    }
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

    /// The shared load, failure, or still-referenced package of a cache entry. Reusing a package
    /// counts as using it, even if it is no longer among the recently used packages.
    fn cached_package(
        &self,
        id: &PackageId<'static>,
        entry: &LoadedPackageEntry,
    ) -> Option<LoadedPackageFuture> {
        match entry {
            LoadedPackageEntry::Loading(future) => Some(future.clone()),
            LoadedPackageEntry::Loaded(package) => {
                let package = package.upgrade()?;
                self.recent_packages.lock().unwrap().touch(id, &package);
                Some(futures::future::ready(Ok(package)).boxed().shared())
            }
            LoadedPackageEntry::Failed(error) => {
                Some(futures::future::ready(Err(error.clone())).boxed().shared())
            }
        }
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
    use assert_fs::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_workspace() -> Arc<Workspace> {
        test_workspace_keeping(0)
    }

    /// A workspace keeping `recent` packages per generation; zero leaves only the weak cache.
    fn test_workspace_keeping(recent: usize) -> Arc<Workspace> {
        Arc::new(Workspace {
            path: PathBuf::new(),
            invocation_options: Arc::new(InvocationOptions {
                ignore_dev_dependency: false,
            }),
            repositories: RwLock::new(HashMap::new()),
            loaded_deps: RwLock::new(HashMap::new()),
            loaded_packages: RwLock::new(HashMap::new()),
            recent_packages: Mutex::new(RecentPackages::new(recent)),
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
        let weak = Arc::downgrade(&first);
        let cached = workspace
            .get_or_add_package(PackageId::new(MAIN_REPO, "cached"), |_| async {
                anyhow::bail!("a live package should be reused")
            })
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&first, &cached));
        drop(first);
        drop(second);
        assert!(weak.upgrade().is_some());
        drop(cached);
        assert!(weak.upgrade().is_none());

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

    #[tokio::test]
    async fn loaded_target_keeps_package_cached_until_last_owner_drops() {
        let workspace = test_workspace();
        let id = PackageId::new(MAIN_REPO, "cached");
        let package = workspace
            .get_or_add_package(id.clone(), |id| async move {
                let mut builder = PackageBuilder::new(id, "BUILD.bazel")?;
                builder.export_source_file("dependency.txt", Vec::new())?;
                Ok(Arc::new(builder.finish(&HashMap::new())?))
            })
            .await
            .unwrap();
        let weak = Arc::downgrade(&package);
        let loaded = workspace
            .load_target(&id.label("BUILD.bazel"))
            .await
            .unwrap()
            .unwrap();
        drop(package);
        assert!(weak.upgrade().is_some());

        let dependency = workspace
            .load_target(&id.label("dependency.txt"))
            .await
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&loaded._package, &dependency._package));

        let expanded = {
            let cloned = loaded.clone();
            drop(loaded);
            drop(dependency);
            assert!(weak.upgrade().is_some());
            assert!(matches!(cloned.target, Target::SourceFile(_)));
            cloned.expanded
        };
        assert_eq!(expanded.label, id.label("BUILD.bazel"));
        assert!(weak.upgrade().is_none());
    }

    #[tokio::test]
    async fn recently_used_packages_stay_loaded() {
        let workspace = test_workspace_keeping(2);
        let loads = Arc::new(AtomicUsize::new(0));
        let load = |name: &'static str| {
            let loads = loads.clone();
            let workspace = workspace.clone();
            async move {
                let package = workspace
                    .get_or_add_package(PackageId::new(MAIN_REPO, name), move |id| async move {
                        loads.fetch_add(1, Ordering::SeqCst);
                        Ok(empty_package(id))
                    })
                    .await
                    .unwrap();
                Arc::downgrade(&package)
            }
        };

        let popular = load("popular").await;
        let unused = load("unused").await;
        let third = load("third").await;
        let fourth = load("fourth").await;
        assert_eq!(loads.load(Ordering::SeqCst), 4);
        assert!(popular.upgrade().is_some() && unused.upgrade().is_some());

        // Reusing a package keeps it loaded, while one unused for a whole generation is released.
        assert!(load("popular").await.ptr_eq(&popular));
        assert_eq!(loads.load(Ordering::SeqCst), 4);
        assert!(unused.upgrade().is_none());
        assert!(popular.upgrade().is_some());
        assert!(third.upgrade().is_some() && fourth.upgrade().is_some());

        // A package referenced elsewhere is reused even after leaving the recent packages.
        let held = load("held").await.upgrade().unwrap();
        for name in ["fifth", "sixth", "seventh", "eighth"] {
            load(name).await;
        }
        let loads_before = loads.load(Ordering::SeqCst);
        assert!(Arc::ptr_eq(&held, &load("held").await.upgrade().unwrap()));
        assert_eq!(loads.load(Ordering::SeqCst), loads_before);
    }

    #[tokio::test]
    async fn recursive_expansion_keeps_only_recent_packages() {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("MODULE.bazel")
            .write_str("module(name = \"package_cache\")\n")
            .unwrap();
        let paths = ["", "a", "b", "c"];
        for path in paths {
            temp.child(format!("{path}/BUILD.bazel").trim_start_matches('/'))
                .write_str("filegroup(name = \"group\")\n")
                .unwrap();
        }
        let workspace = Workspace::new(
            temp.path(),
            Arc::new(InvocationOptions {
                ignore_dev_dependency: false,
            }),
        )
        .await
        .unwrap();
        *workspace.recent_packages.lock().unwrap() = RecentPackages::new(1);
        let mut targets = workspace.clone().expand_pattern(TargetPattern {
            repo: Repo::Canonical(MAIN_REPO),
            package: Cow::Borrowed(""),
            target_kind: TargetPatternKind::AllRules,
            include_subpackages: true,
        });
        let mut packages: Vec<Weak<Package>> = Vec::new();
        let mut expanded = Vec::new();
        for path in paths {
            let target = targets.next().await.unwrap().unwrap();
            assert_eq!(target.label, CanonicalLabel::new(MAIN_REPO, path, "group"));
            let loaded = workspace.loaded_packages.read().unwrap();
            let Some(LoadedPackageEntry::Loaded(package)) =
                loaded.get(&PackageId::new(MAIN_REPO, path))
            else {
                panic!("expected loaded package `{path}`");
            };
            packages.push(package.clone());
            drop(loaded);
            // With one package per generation, only the current and previous packages stay
            // loaded, even while expanded targets are still held.
            let live = packages
                .iter()
                .map(|package| package.upgrade().is_some())
                .collect::<Vec<_>>();
            let kept = live.len().min(2);
            assert!(
                live[..live.len() - kept].iter().all(|live| !live),
                "{live:?}"
            );
            assert!(
                live[live.len() - kept..].iter().all(|live| *live),
                "{live:?}"
            );
            expanded.push(target);
        }
        assert!(targets.next().await.is_none());
        assert_eq!(expanded.len(), paths.len());
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
