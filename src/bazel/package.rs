#![allow(dead_code, unused)]

use std::pin::Pin;

use tokio::io;

pub use bazel_remote_apis::build::bazel::remote::execution::v2::Digest;
pub use bazel_remote_apis::build::bazel::remote::execution::v2::digest_function::Value as DigestFunction;

pub enum DirEntry {
    File(String),
    Directory(String),
}

// A package is a directory with a BUILD file.
#[derive(Debug)]
pub struct Package<F: FileStore> {
    pub path: String,
    pub build_file_name: String,
    _build_file: F::File,
    pub filestore: F,
}

impl<F: FileStore> Package<F> {
    pub fn new(path: String, build_file_name: String, filestore: F, build_file: F::File) -> Self {
        Self {
            path,
            build_file_name,
            _build_file: build_file,
            filestore,
        }
    }

    pub fn subpackages<'a>(&'a self) -> futures::stream::BoxStream<'a, anyhow::Result<Package<F>>>
    where
        F: Clone + 'a,
    {
        Box::pin(async_stream::try_stream! {
            let mut stack = vec![self.path.clone()];

            while let Some(current_dir) = stack.pop() {
                // Read the directory contents
                let mut dir_entries = match self.filestore.read_dir(&current_dir).await {
                    Ok(entries) => entries,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => {
                        yield Err(anyhow::Error::from(e))?;
                        continue;
                    }
                };

                let mut subdirs = Vec::new();
                let mut build_file_name = None;

                for entry in dir_entries {
                    match entry {
                        DirEntry::Directory(name) => {
                            let name_str = name.as_str();
                            if name_str.starts_with('.') || name_str == "target" || name_str.starts_with("bazel-") {
                                continue;
                            }
                            let next_path = if current_dir.is_empty() {
                                name
                            } else {
                                format!("{}/{}", current_dir, name)
                            };
                            subdirs.push(next_path);
                        }
                        DirEntry::File(name) => {
                            if name == "BUILD.bazel" {
                                build_file_name = Some("BUILD.bazel".to_string());
                            } else if name == "BUILD" && build_file_name.is_none() {
                                build_file_name = Some("BUILD".to_string());
                            }
                        }
                    }
                }

                // Add subdirectories to the stack to continue walking
                // (We do this even if we found a BUILD file here, as Bazel //... walks past package boundaries)
                stack.extend(subdirs);

                // If this directory has a BUILD file, yield it as a subpackage (skip if it's the root package itself)
                if let Some(bf) = build_file_name {
                    if current_dir != self.path {
                        let build_path = if current_dir.is_empty() { bf.clone() } else { format!("{}/{}", current_dir, bf) };
                        match self.filestore.read_file(&build_path).await {
                            Ok(file) => {
                                yield Package::new(current_dir, bf, self.filestore.clone(), file);
                            }
                            Err(e) => yield Err(anyhow::Error::from(e))?,
                        }
                    }
                }
            }
        })
    }

    // TODO
    // pub async fn targets(&self) -> impl Stream<Item = anyhow::Result<Target>> {
    //     // TODO: Implement this
    //     vec![]
    // }

    pub async fn source_files(&self) -> impl Stream<Item = anyhow::Result<String>> {
        // TODO: Implement this
        futures::stream::once(futures::future::ready(Err(anyhow::anyhow!(
            "Not implemented"
        ))))
    }
}

use futures::{
    Stream,
    future::{BoxFuture, FutureExt},
};

pub type BoxAsyncRead = Box<dyn io::AsyncRead + Unpin + Send>;
pub type BoxFile<'a> = Box<DynFile<'a, BoxAsyncRead>>;
pub type BoxFileStore<'a> = std::sync::Arc<DynFileStore<'a, BoxFile<'a>>>;

#[dynosaur::dynosaur(pub DynFile = dyn(box) File)]
pub trait File: Send + Sync + std::fmt::Debug {
    type AsyncRead: io::AsyncRead;

    /// Get the file contents as an async reader.
    fn open(&self) -> BoxFuture<'_, Result<Self::AsyncRead, std::io::Error>>;

    /// Get the file digest.
    fn digest(
        &self,
        digest_function: DigestFunction,
    ) -> BoxFuture<'_, Result<Digest, std::io::Error>>;
}

impl<'a, R: io::AsyncRead> std::fmt::Debug for DynFile<'a, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynFile").finish_non_exhaustive()
    }
}

#[dynosaur::dynosaur(pub DynFileStore = dyn(box) FileStore)]
pub trait FileStore: Send + Sync + std::fmt::Debug {
    type File: crate::bazel::package::File;

    /// Read a file from the repository.
    ///
    /// The path is relative to the repository root.
    fn read_file(&self, path: &str) -> BoxFuture<'_, Result<Self::File, std::io::Error>>;

    /// Read a directory within the repository.
    ///
    /// The path is relative to the repository root.
    fn read_dir(&self, path: &str) -> BoxFuture<'_, Result<Vec<DirEntry>, std::io::Error>>;
}

impl<'a, F: File> std::fmt::Debug for DynFileStore<'a, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynFileStore").finish_non_exhaustive()
    }
}

// Wrapper to force type erasure of the AsyncRead associated type
#[derive(Debug)]
struct FileAdapter<F>(F);

impl<F: File> File for FileAdapter<F>
where
    F::AsyncRead: Unpin + Send + 'static,
{
    type AsyncRead = BoxAsyncRead;

    fn open(&self) -> BoxFuture<'_, Result<Self::AsyncRead, std::io::Error>> {
        async move {
            let r = self.0.open().await?;
            Ok(Box::new(r) as BoxAsyncRead)
        }
        .boxed()
    }

    fn digest(
        &self,
        digest_function: DigestFunction,
    ) -> BoxFuture<'_, Result<Digest, std::io::Error>> {
        self.0.digest(digest_function).boxed()
    }
}

impl<F: FileStore + ?Sized> FileStore for std::sync::Arc<F> {
    type File = F::File;

    fn read_file(&self, path: &str) -> BoxFuture<'_, Result<Self::File, std::io::Error>> {
        (**self).read_file(path)
    }

    fn read_dir(&self, path: &str) -> BoxFuture<'_, Result<Vec<DirEntry>, std::io::Error>> {
        (**self).read_dir(path)
    }
}

#[derive(Debug)]
pub struct TypeErasingFileStore<F>(pub F);

impl<F: FileStore> FileStore for TypeErasingFileStore<F>
where
    F::File: 'static,
    <<F as FileStore>::File as File>::AsyncRead: Unpin + Send + 'static,
{
    type File = BoxFile<'static>;

    fn read_file(&self, path: &str) -> BoxFuture<'_, std::io::Result<Self::File>> {
        let path = path.to_string();
        async move {
            self.0
                .read_file(&path)
                .await
                .map(|f| DynFile::new_box(FileAdapter(f)))
        }
        .boxed()
    }

    fn read_dir(&self, path: &str) -> BoxFuture<'_, std::io::Result<Vec<DirEntry>>> {
        self.0.read_dir(path).boxed()
    }
}
