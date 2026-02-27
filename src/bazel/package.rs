#![allow(dead_code, unused)]

use std::pin::Pin;

use tokio::io;

pub use bazel_remote_apis::build::bazel::remote::execution::v2::Digest;
pub use bazel_remote_apis::build::bazel::remote::execution::v2::digest_function::Value as DigestFunction;

// A package is a directory with a BUILD file.
#[derive(Debug)]
pub struct Package<F: FileStore> {
    _build_file: F::File,
    _filestore: F,
}

impl<F: FileStore> Package<F> {
    pub fn new(filestore: F, build_file: F::File) -> Self {
        Self {
            _build_file: build_file,
            _filestore: filestore,
        }
    }

    pub fn sub_packages(&self) -> Vec<String> {
        // TODO: Implement this
        vec![]
    }
}

use futures::future::{BoxFuture, FutureExt};

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
    fn read_dir(&self, path: &str) -> BoxFuture<'_, Result<Vec<String>, std::io::Error>>;
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

    fn read_dir(&self, path: &str) -> BoxFuture<'_, Result<Vec<String>, std::io::Error>> {
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

    fn read_dir(&self, path: &str) -> BoxFuture<'_, std::io::Result<Vec<String>>> {
        self.0.read_dir(path).boxed()
    }
}
