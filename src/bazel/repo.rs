#![allow(dead_code, unused)]

use futures::future::{BoxFuture, FusedFuture, FutureExt};

use crate::{
    bazel::{
        label::{ApparentRepo, CanonicalRepo, MAIN_REPO},
        package::{
            BoxFile, BoxedFileStore, Digest, DigestFunction, DynFileStore, File, FileStore, Package,
        },
    },
    workspace::Workspace,
};
use std::collections::HashMap;
use tokio::fs;
use tokio::io::AsyncReadExt;

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
    repo_mapping: HashMap<ApparentRepo<'a>, CanonicalRepo<'a>>,
    files: BoxedFileStore<'a>,
    // TODO: include info from REPO.bazel, and use in read_package()
}

impl<'a> Repository<'a> {
    pub async fn new(
        workspace: std::sync::Arc<Workspace>,
        canonical_name: CanonicalRepo<'static>,
        files: BoxedFileStore<'static>,
    ) -> anyhow::Result<Repository<'static>>
    where
        'a: 'static,
    {
        let is_root = canonical_name == MAIN_REPO;
        // Pass the file store to eval_module to handle reading MODULE.bazel and includes
        let module = crate::bazel::bzlmod::eval_module(&files, "MODULE.bazel", is_root).await?;

        if is_root {
            // TODO: copy overrides into Workspace
        }

        let mut repo_mapping = HashMap::with_capacity(module.bazel_deps.len());
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
            repo_mapping,
            files,
        })
    }

    pub fn canonical_name(&self) -> CanonicalRepo<'a> {
        self.canonical_name.clone()
    }

    pub fn files(&self) -> &BoxedFileStore<'a> {
        &self.files
    }

    pub async fn read_package(
        &self,
        pkg: &str,
    ) -> Result<Package<BoxedFileStore<'a>>, std::io::Error> {
        // Bazel looks for BUILD.bazel first, then BUILD. It's an error if both exist.
        // We read both in parallel to maximize performance.
        let build_bazel_path = format!("{pkg}/BUILD.bazel");
        let build_path = format!("{pkg}/BUILD");

        let (build_bazel_result, build_result) = tokio::join!(
            self.read_file(&build_bazel_path),
            self.read_file(&build_path)
        );

        match (build_bazel_result, build_result) {
            // Success case 1: BUILD.bazel exists, BUILD does not.
            (Ok(file), Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Package::new(self.files.clone(), file))
            }
            // Success case 2: BUILD.bazel does not exist, BUILD does.
            (Err(e), Ok(file)) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Package::new(self.files.clone(), file))
            }
            // Error case 1: Both exist.
            (Ok(_), Ok(_)) => Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("Package '{pkg}' contains both BUILD and BUILD.bazel files."),
            )),
            // Error case 2: Neither exists.
            (Err(e1), Err(e2))
                if e1.kind() == std::io::ErrorKind::NotFound
                    && e2.kind() == std::io::ErrorKind::NotFound =>
            {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Package '{pkg}' not found: missing BUILD or BUILD.bazel file."),
                ))
            }
            // Propagate other errors. If BUILD.bazel read had an error, propagate it first.
            (Err(e), _) => Err(e),
            // Otherwise, propagate the error from reading BUILD.
            (_, Err(e)) => Err(e),
        }
    }

    pub async fn read_file(&self, path: &str) -> Result<BoxFile<'a>, std::io::Error> {
        self.files.read_file(path).await
    }

    pub async fn read_dir(&self, path: &str) -> Result<Vec<String>, std::io::Error> {
        self.files.read_dir(path).await
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

    fn read_dir(&self, path: &str) -> BoxFuture<'_, Result<Vec<String>, std::io::Error>> {
        let full_path = self.root.join(path);
        async move {
            let mut read_dir = fs::read_dir(full_path).await?;
            let mut results = Vec::new();

            while let Some(entry) = read_dir.next_entry().await? {
                results.push(entry.file_name().to_string_lossy().to_string());
            }

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
    files: std::collections::HashMap<String, Vec<u8>>,
}

impl InMemoryFileStore {
    pub fn new(files: std::collections::HashMap<String, Vec<u8>>) -> Self {
        Self { files }
    }
}

impl FileStore for InMemoryFileStore {
    type File = InMemoryFile;

    fn read_file(&self, path: &str) -> BoxFuture<'_, Result<Self::File, std::io::Error>> {
        // TODO: fix return lifetimes so we can use &self and delay lookup until async body is executed.
        let content = self.files.get(path).cloned();
        let path_str = path.to_string();
        async move {
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

    fn read_dir(&self, path: &str) -> BoxFuture<'_, Result<Vec<String>, std::io::Error>> {
        let dir_path = std::path::PathBuf::from(path);
        // Need to clone necessary data for async block.
        // TODO: fix return lifetimes so this clone is unnecessary.
        let keys: Vec<String> = self.files.keys().cloned().collect();

        async move {
            let results: std::collections::HashSet<_> = keys
                .iter()
                .filter_map(|file_path_str| {
                    std::path::Path::new(file_path_str)
                        .strip_prefix(&dir_path)
                        .ok()
                        .and_then(|p| p.components().next())
                        .and_then(|c| match c {
                            std::path::Component::Normal(name) => {
                                Some(name.to_string_lossy().into_owned())
                            }
                            _ => None,
                        })
                })
                .collect();
            Ok(results.into_iter().collect())
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
    use std::collections::HashMap;
    use tokio::io::{self};

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
        let mut root_entries = store.read_dir("").await.unwrap();
        root_entries.sort();
        assert_eq!(root_entries, vec!["a", "f"]);

        // Test subdirectory "a" (with and without trailing slash)
        for path in ["a", "a/"] {
            let mut entries = store.read_dir(path).await.unwrap();
            entries.sort();
            assert_eq!(entries, vec!["b", "c", "d"], "Failed for path: {path}");
        }

        // Test deeper subdirectory "a/d"
        let mut ad_entries = store.read_dir("a/d").await.unwrap();
        ad_entries.sort();
        assert_eq!(ad_entries, vec!["e"]);

        // Test non-existent directory
        let none_entries = store.read_dir("nonexistent").await.unwrap();
        assert!(none_entries.is_empty());
    }

    #[tokio::test]
    async fn test_type_erased_map() {
        // Create a map of type-erased FileStores
        let mut map: HashMap<String, BoxedFileStore> = HashMap::new();

        // Add LocalFileStore
        let local_store = LocalFileStore::new(std::path::PathBuf::from("/tmp"));
        // Explicitly box the store to use the manual bridge implementation
        // Box<LocalFileStore> implements FileStore<File=BoxFile>
        let boxed_local: BoxedFileStore = std::sync::Arc::from(DynFileStore::new_box(Box::new(
            crate::bazel::package::TypeErasingFileStore(local_store),
        )));
        map.insert("local".to_string(), boxed_local);

        // Add InMemoryFileStore
        let memory_files = HashMap::from([("foo".to_string(), b"bar".to_vec())]);
        let memory_store = InMemoryFileStore::new(memory_files);
        // InMemoryFileStore -> Box<InMemoryFileStore> -> FileStore -> DynFileStore -> Box<DynFileStore>
        let boxed_memory: BoxedFileStore<'static> = std::sync::Arc::from(DynFileStore::new_box(
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
