#![allow(dead_code, unused)]

use futures::future::{BoxFuture, FusedFuture, FutureExt};
use futures::stream::BoxStream;

use crate::{
    bazel::{
        label::{ApparentRepo, CanonicalLabel, CanonicalRepo, Label, MAIN_REPO},
        package::{
            BoxFile, BoxFileStore, Digest, DigestFunction, DirEntry, DynFileStore, File, FileStore,
            Package, PackageId, PackageSource,
        },
    },
    workspace::Workspace,
};
use std::collections::{HashMap, HashSet};
use tokio::fs;
use tokio::io::AsyncReadExt;

#[derive(Debug)]
pub(crate) struct PackageNotFound {
    package: String,
}

impl std::fmt::Display for PackageNotFound {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Package '{}' not found: missing BUILD or BUILD.bazel file",
            self.package
        )
    }
}

impl std::error::Error for PackageNotFound {}

#[derive(Debug, Clone)]
pub(crate) struct RepositoryMapping<'a> {
    repo_mapping: HashMap<ApparentRepo<'a>, CanonicalRepo<'a>>,
}

impl<'a> RepositoryMapping<'a> {
    fn resolve_repo<'repo>(
        &'repo self,
        apparent: &ApparentRepo<'repo>,
    ) -> Option<CanonicalRepo<'repo>>
    where
        'a: 'repo,
    {
        self.repo_mapping
            .get(apparent)
            .map(CanonicalRepo::as_borrowed)
    }

    pub fn resolve_label<'repo>(&'repo self, label: Label<'repo>) -> Option<CanonicalLabel<'repo>>
    where
        'a: 'repo,
    {
        label.into_canonical(|apparent| self.resolve_repo(apparent))
    }
}

fn entry_name(entry: &DirEntry) -> &str {
    match entry {
        DirEntry::File(name) | DirEntry::Directory(name) => name,
    }
}

/// A directory tree with a boundary marker file at its root, containing source files that can be used in a Bazel build. Often shortened to just repo.
///
/// A repo boundary marker file can be `MODULE.bazel` (signaling that this repo represents a Bazel module), `REPO.bazel` (see below), or in legacy contexts, `WORKSPACE` or `WORKSPACE.bazel`. Any repo boundary marker file will signify the boundary of a repo; multiple such files can coexist in a directory.
///
/// The "main" repository is the repository in which the current Bazel command is being run.
/// The root of the main repository is also known as the _workspace root_.
#[derive(Debug)]
pub struct Repository<'a> {
    repo_name: ApparentRepo<'a>,
    canonical_name: CanonicalRepo<'a>,
    repo_mapping: RepositoryMapping<'a>,
    files: BoxFileStore<'a>,
    // TODO: include info from REPO.bazel, and use in read_package()
}

impl<'a> Repository<'a> {
    pub async fn new(
        workspace: std::sync::Arc<Workspace>,
        canonical_name: CanonicalRepo<'static>,
        files: BoxFileStore<'static>,
    ) -> anyhow::Result<Repository<'static>>
    where
        'a: 'static,
    {
        let is_root = canonical_name == MAIN_REPO;
        // Pass the file store to eval_module to handle reading MODULE.bazel and includes
        let module = crate::bazel::bzlmod::eval_module(
            &files,
            "MODULE.bazel",
            is_root,
            workspace.invocation_options().ignore_dev_dependency,
        )
        .await?;

        if is_root {
            // TODO: copy overrides into Workspace
        }

        let mut repo_mapping = HashMap::with_capacity(module.bazel_deps.len() + 1);
        repo_mapping.insert(ApparentRepo::new(""), canonical_name.clone());
        for dep in module.bazel_deps {
            // TODO: this should go via a Workspace method so we can pick up overrides.

            let canonical_name = CanonicalRepo::new(format!("{}+{}", dep.name, dep.version));
            repo_mapping.insert(
                ApparentRepo::new(dep.repo_name.clone()),
                canonical_name.clone(),
            );

            // Create Repository in Workspace (if it doesn't already exist)
            // TODO: This bit should move into a method on Workspace
            let repo = async {
                anyhow::bail!("Not implemented");
            };
            workspace.add_repository(canonical_name, repo);
        }

        let repo_name = ApparentRepo::new(module.repo_name);

        Ok(Self {
            repo_name,
            canonical_name,
            repo_mapping: RepositoryMapping { repo_mapping },
            files,
        })
    }

    pub fn canonical_name(&self) -> &CanonicalRepo<'a> {
        &self.canonical_name
    }

    pub(crate) fn mapping(&self) -> &RepositoryMapping<'a> {
        &self.repo_mapping
    }

    /// Resolves an apparent repository name in this repository's mapping.
    pub fn resolve_repo<'repo>(
        &'repo self,
        apparent: &ApparentRepo<'repo>,
    ) -> Option<CanonicalRepo<'repo>>
    where
        'a: 'repo,
    {
        self.repo_mapping.resolve_repo(apparent)
    }

    /// Resolves the apparent repository portion of a label in this repository's mapping.
    pub fn resolve_label<'repo>(&'repo self, label: Label<'repo>) -> Option<CanonicalLabel<'repo>>
    where
        'a: 'repo,
    {
        self.repo_mapping.resolve_label(label)
    }

    pub fn files(&self) -> &BoxFileStore<'a> {
        &self.files
    }

    pub async fn read_package(
        &self,
        pkg: &str,
    ) -> Result<PackageSource<BoxFileStore<'a>>, std::io::Error> {
        let build_bazel_path = if pkg.is_empty() {
            "BUILD.bazel".to_string()
        } else {
            format!("{pkg}/BUILD.bazel")
        };
        match self.read_file(&build_bazel_path).await {
            Ok(file) => Ok(PackageSource::new(
                pkg.to_string(),
                "BUILD.bazel".to_string(),
                self.files.clone(),
                file,
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let build_path = if pkg.is_empty() {
                    "BUILD".to_string()
                } else {
                    format!("{pkg}/BUILD")
                };
                match self.read_file(&build_path).await {
                    Ok(file) => Ok(PackageSource::new(
                        pkg.to_string(),
                        "BUILD".to_string(),
                        self.files.clone(),
                        file,
                    )),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            PackageNotFound {
                                package: pkg.to_owned(),
                            },
                        ))
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    pub fn subpackage_paths<'repo>(
        &'repo self,
        root: &'repo str,
    ) -> BoxStream<'repo, anyhow::Result<String>> {
        Box::pin(async_stream::try_stream! {
            let mut stack = vec![root.to_owned()];
            while let Some(current_dir) = stack.pop() {
                if self.canonical_name == MAIN_REPO
                    && (current_dir == "external" || current_dir.starts_with("external/"))
                {
                    continue;
                }
                let mut entries = match self.read_dir(&current_dir).await {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => Err(error)?,
                };
                entries.sort_by(|left, right| entry_name(left).cmp(entry_name(right)));
                let has_build = entries.iter().any(|entry| {
                    matches!(entry, DirEntry::File(name) if name == "BUILD.bazel" || name == "BUILD")
                });
                let mut subdirectories = entries
                    .into_iter()
                    .filter_map(|entry| match entry {
                        DirEntry::Directory(name) => {
                            Some(if current_dir.is_empty() {
                                name
                            } else {
                                format!("{current_dir}/{name}")
                            })
                        }
                        DirEntry::File(_) => None,
                    })
                    .collect::<Vec<_>>();
                subdirectories.reverse();
                stack.extend(subdirectories);
                if current_dir != root && has_build {
                    yield current_dir;
                }
            }
        })
    }

    pub async fn read_file(&self, path: &str) -> Result<BoxFile<'a>, std::io::Error> {
        self.files.read_file(path).await
    }

    pub async fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>, std::io::Error> {
        self.files.read_dir(path).await
    }

    pub async fn eval_package(
        self: &std::sync::Arc<Self>,
        pkg: &PackageSource<BoxFileStore<'a>>,
        id: PackageId<'static>,
        workspace: std::sync::Arc<Workspace>,
    ) -> anyhow::Result<Package>
    where
        'a: 'static,
    {
        crate::starlark::eval::eval_build(workspace, self.clone(), id, pkg).await
    }
}

// Concrete FileStore implementations
#[derive(Debug, Clone)]
pub struct LocalFileStore {
    root: std::path::PathBuf,
}

impl LocalFileStore {
    pub fn new(root: std::path::PathBuf) -> Self {
        Self { root }
    }
}

impl FileStore for LocalFileStore {
    type File = LocalFile;

    fn read_file(&self, path: &str) -> BoxFuture<'_, Result<Self::File, std::io::Error>> {
        let full_path = self.root.join(path);
        async move {
            // Ensure path exists
            if !full_path.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("File not found: {:?}", full_path),
                ));
            }
            Ok(LocalFile::new(full_path))
        }
        .boxed()
    }

    fn read_dir(&self, path: &str) -> BoxFuture<'_, Result<Vec<DirEntry>, std::io::Error>> {
        let full_path = self.root.join(path);
        async move {
            let mut read_dir = fs::read_dir(full_path).await?;
            let mut results = Vec::new();

            while let Some(entry) = read_dir.next_entry().await? {
                let name = entry.file_name().to_string_lossy().to_string();
                if entry.file_type().await?.is_dir() {
                    results.push(DirEntry::Directory(name));
                } else {
                    results.push(DirEntry::File(name));
                }
            }

            results.sort_by(|left, right| entry_name(left).cmp(entry_name(right)));

            Ok(results)
        }
        .boxed()
    }
}

#[derive(Debug)]
pub struct LocalFile {
    path: std::path::PathBuf,
}

impl LocalFile {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }
}

impl File for LocalFile {
    type AsyncRead = fs::File;

    fn open(&self) -> BoxFuture<'_, Result<Self::AsyncRead, std::io::Error>> {
        let path = self.path.clone();
        async move { fs::File::open(path).await }.boxed()
    }

    fn digest(
        &self,
        _digest_function: DigestFunction,
    ) -> BoxFuture<'_, Result<Digest, std::io::Error>> {
        async move { todo!("Implement digest for LocalFile") }.boxed()
    }
}

#[derive(Debug, Clone)]
pub struct InMemoryFileStore {
    files: HashMap<String, Vec<u8>>,
}

impl InMemoryFileStore {
    pub fn new(files: HashMap<String, Vec<u8>>) -> Self {
        Self { files }
    }
}

impl FileStore for InMemoryFileStore {
    type File = InMemoryFile;

    fn read_file(&self, path: &str) -> BoxFuture<'_, Result<Self::File, std::io::Error>> {
        let path_str = path.to_string();
        async move {
            // This files.get() is deliberately delayed until the future executes, since it represents the "expensive" read_file operation.
            let content = self.files.get(&path_str).cloned();
            if let Some(content) = content {
                Ok(InMemoryFile { content })
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("File not found: {}", path_str),
                ))
            }
        }
        .boxed()
    }

    fn read_dir(&self, path: &str) -> BoxFuture<'_, Result<Vec<DirEntry>, std::io::Error>> {
        let dir_path = std::path::PathBuf::from(path);
        async move {
            let mut entries = HashMap::new();
            for file_path_str in self.files.keys() {
                if let Ok(stripped) = std::path::Path::new(file_path_str).strip_prefix(&dir_path) {
                    let mut components = stripped.components();
                    if let Some(std::path::Component::Normal(name)) = components.next() {
                        let name_str = name.to_string_lossy().into_owned();
                        if components.next().is_some() {
                            entries.insert(name_str, true);
                        } else {
                            entries.entry(name_str).or_insert(false);
                        }
                    }
                }
            }
            let mut entries = entries
                .into_iter()
                .map(|(name, is_directory)| {
                    if is_directory {
                        DirEntry::Directory(name)
                    } else {
                        DirEntry::File(name)
                    }
                })
                .collect::<Vec<_>>();
            entries.sort_by(|left, right| entry_name(left).cmp(entry_name(right)));
            Ok(entries)
        }
        .boxed()
    }
}

#[derive(Debug, Clone)]
pub struct InMemoryFile {
    content: Vec<u8>,
}

impl File for InMemoryFile {
    type AsyncRead = std::io::Cursor<Vec<u8>>;

    fn open(&self) -> BoxFuture<'_, Result<Self::AsyncRead, std::io::Error>> {
        let content = self.content.clone();
        async move { Ok(std::io::Cursor::new(content)) }.boxed()
    }

    fn digest(
        &self,
        _digest_function: DigestFunction,
    ) -> BoxFuture<'_, Result<Digest, std::io::Error>> {
        async move { todo!("Implement digest for InMemoryFile") }.boxed()
    }
}

#[cfg(test)]
pub mod test {
    use super::*;
    use crate::bazel::label::Repo;
    use futures::StreamExt;
    use std::collections::HashMap;
    use tokio::io::{self};

    #[test]
    fn test_resolve_repo_and_label() {
        let files: BoxFileStore<'static> = std::sync::Arc::from(DynFileStore::new_box(Box::new(
            crate::bazel::package::TypeErasingFileStore(InMemoryFileStore::new(HashMap::new())),
        )));
        let canonical_name = CanonicalRepo::new(String::from("root+1.0"));
        let canonical_dep = CanonicalRepo::new(String::from("dep+1.0"));
        let repo = Repository {
            repo_name: ApparentRepo::new("root"),
            canonical_name: canonical_name.clone(),
            repo_mapping: RepositoryMapping {
                repo_mapping: HashMap::from([
                    (ApparentRepo::new(""), canonical_name.clone()),
                    (ApparentRepo::new("dep_alias"), canonical_dep.clone()),
                ]),
            },
            files,
        };

        let apparent_name = String::from("dep_alias");
        let stored = repo
            .mapping()
            .repo_mapping
            .get(&ApparentRepo::new("dep_alias"))
            .unwrap();
        let resolved = repo
            .resolve_repo(&ApparentRepo::new(apparent_name.as_str()))
            .unwrap();
        assert_eq!(resolved, canonical_dep);
        assert!(std::ptr::eq(resolved.as_str(), stored.as_str()));
        assert_eq!(repo.resolve_repo(&ApparentRepo::new("unknown")), None);
        assert_eq!(
            repo.resolve_repo(&ApparentRepo::new("")),
            Some(canonical_name.clone())
        );

        let current_repo_label =
            Label::new(Repo::Apparent(ApparentRepo::new("")), "package", "target");
        assert_eq!(
            repo.resolve_label(current_repo_label).unwrap().repo,
            canonical_name
        );

        let apparent_label = Label::new(
            Repo::Apparent(ApparentRepo::new(apparent_name.as_str())),
            "package",
            "target",
        );
        let resolved_label = repo.resolve_label(apparent_label).unwrap();
        assert_eq!(resolved_label.repo, canonical_dep);
        assert_eq!(resolved_label.package, "package");
        assert_eq!(resolved_label.target, "target");

        let canonical_label = Label::new(
            Repo::Canonical(CanonicalRepo::new("already_canonical")),
            "package",
            "target",
        );
        assert_eq!(
            repo.resolve_label(canonical_label).unwrap().repo,
            CanonicalRepo::new("already_canonical")
        );

        let unknown_label = Label::new(
            Repo::Apparent(ApparentRepo::new("unknown")),
            "package",
            "target",
        );
        assert!(repo.resolve_label(unknown_label).is_none());
    }

    #[tokio::test]
    async fn test_in_memory_file_store_read_dir() {
        let files = HashMap::from([
            ("a/b".to_string(), vec![]),
            ("a/c".to_string(), vec![]),
            ("a/d/e".to_string(), vec![]),
            ("f".to_string(), vec![]),
        ]);

        let store = InMemoryFileStore::new(files);

        // Test root directory
        let mut root_entries: Vec<String> = store
            .read_dir("")
            .await
            .unwrap()
            .into_iter()
            .map(|e| match e {
                DirEntry::File(s) => s,
                DirEntry::Directory(s) => s,
            })
            .collect();
        root_entries.sort();
        assert_eq!(root_entries, vec!["a", "f"]);

        // Test subdirectory "a" (with and without trailing slash)
        for path in ["a", "a/"] {
            let mut entries: Vec<String> = store
                .read_dir(path)
                .await
                .unwrap()
                .into_iter()
                .map(|e| match e {
                    DirEntry::File(s) => s,
                    DirEntry::Directory(s) => s,
                })
                .collect();
            entries.sort();
            assert_eq!(entries, vec!["b", "c", "d"], "Failed for path: {path}");
        }

        // Test deeper subdirectory "a/d"
        let mut ad_entries: Vec<String> = store
            .read_dir("a/d")
            .await
            .unwrap()
            .into_iter()
            .map(|e| match e {
                DirEntry::File(s) => s,
                DirEntry::Directory(s) => s,
            })
            .collect();
        ad_entries.sort();
        assert_eq!(ad_entries, vec!["e"]);

        // Test non-existent directory
        let none_entries = store.read_dir("nonexistent").await.unwrap();
        assert!(none_entries.is_empty());
    }

    #[tokio::test]
    async fn read_package_prefers_build_bazel() {
        let files = HashMap::from([
            ("BUILD".to_owned(), b"legacy".to_vec()),
            ("BUILD.bazel".to_owned(), b"preferred".to_vec()),
        ]);
        let files: BoxFileStore<'static> = std::sync::Arc::from(DynFileStore::new_box(Box::new(
            crate::bazel::package::TypeErasingFileStore(InMemoryFileStore::new(files)),
        )));
        let repository = Repository {
            repo_name: ApparentRepo::new("root"),
            canonical_name: MAIN_REPO,
            repo_mapping: RepositoryMapping {
                repo_mapping: HashMap::new(),
            },
            files,
        };

        let package = repository.read_package("").await.unwrap();
        assert_eq!(package.build_file_name(), "BUILD.bazel");
    }

    #[tokio::test]
    async fn recursive_discovery_includes_ordinary_hidden_and_tool_directories() {
        let files = HashMap::from([
            (".hidden/BUILD.bazel".to_owned(), Vec::new()),
            ("target/BUILD.bazel".to_owned(), Vec::new()),
            ("bazel-custom/BUILD.bazel".to_owned(), Vec::new()),
            ("external/BUILD.bazel".to_owned(), Vec::new()),
        ]);
        let files: BoxFileStore<'static> = std::sync::Arc::from(DynFileStore::new_box(Box::new(
            crate::bazel::package::TypeErasingFileStore(InMemoryFileStore::new(files)),
        )));
        let repository = Repository {
            repo_name: ApparentRepo::new("root"),
            canonical_name: MAIN_REPO,
            repo_mapping: RepositoryMapping {
                repo_mapping: HashMap::new(),
            },
            files,
        };

        let mut packages = repository.subpackage_paths("");
        let mut paths = Vec::new();
        while let Some(path) = packages.next().await {
            paths.push(path.unwrap());
        }
        assert_eq!(paths, [".hidden", "bazel-custom", "target"]);
    }

    #[tokio::test]
    async fn test_type_erased_map() {
        // Create a map of type-erased FileStores
        let mut map: HashMap<String, BoxFileStore> = HashMap::new();

        // Add LocalFileStore
        let local_store = LocalFileStore::new(std::path::PathBuf::from("/tmp"));
        // Explicitly box the store to use the manual bridge implementation
        // Box<LocalFileStore> implements FileStore<File=BoxFile>
        let boxed_local: BoxFileStore = std::sync::Arc::from(DynFileStore::new_box(Box::new(
            crate::bazel::package::TypeErasingFileStore(local_store),
        )));
        map.insert("local".to_string(), boxed_local);

        // Add InMemoryFileStore
        let memory_files = HashMap::from([("foo".to_string(), b"bar".to_vec())]);
        let memory_store = InMemoryFileStore::new(memory_files);
        // InMemoryFileStore -> Box<InMemoryFileStore> -> FileStore -> DynFileStore -> Box<DynFileStore>
        let boxed_memory: BoxFileStore<'static> = std::sync::Arc::from(DynFileStore::new_box(
            Box::new(crate::bazel::package::TypeErasingFileStore(memory_store)),
        ));
        map.insert("memory".to_string(), boxed_memory);

        // Verify retrieval and usage
        let store = map.get("memory").unwrap();
        let file_content = store.read_file("foo").await.unwrap();

        use tokio::io::AsyncReadExt;
        let mut content = Vec::new();
        let mut reader = file_content.open().await.unwrap();
        reader.read_to_end(&mut content).await.unwrap();
        assert_eq!(content, b"bar");
    }
}
