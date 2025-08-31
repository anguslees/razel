use std::sync::Arc;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use std::marker::Unpin;
use std::pin::Pin;

use crate::bazel::Configuration;
use crate::workspace::Workspace;

pub async fn query<W>(out: &mut W, _config: Arc<Configuration>, _query: &str) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
 {
    let workspace = Workspace::new(".")?;
    println!("Workspace path: {:?}", workspace.path());

    let module_path = workspace.path().join("MODULE.bazel");
    let module = crate::bazel::bzlmod::eval_module(&module_path, true).await?;

    println!("MODULE.bazel defined module name {}, repo_name={}, version={}", module.name, module.repo_name, module.version);
    println!("MODULE.bazel defined module {module:?}");

    // Construct repos from bzlmod declarations
    // Global Map of Canonical name -> FusedFuture<dyn Repo>
    // Each repo (including _main) needs a Map of repo name -> Canonical name

    // Parse/execute query.  Simplest is a list of targets.

    out.write_all(b"not real output\n").await?;

    unimplemented!();
    Ok(())
}
