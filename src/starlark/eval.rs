use crate::bazel::label::{CanonicalLabel, Label};
use crate::bazel::package::File;
use crate::bazel::repo::Repository;
use crate::bazel::rule::Rule;
use crate::workspace::Workspace;
use futures::future::{BoxFuture, FutureExt};
use starlark::environment::{FrozenModule, Module as StarlarkModule};
use starlark::eval::{Evaluator, FileLoader};
use starlark::syntax::{AstModule, Dialect};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::AsyncReadExt;

const DIALECT_BUILD: Dialect = Dialect {
    enable_load: true,
    ..Dialect::Standard
};

struct HashMapFileLoader<'a> {
    modules: &'a HashMap<String, FrozenModule>,
}

impl<'a> FileLoader for HashMapFileLoader<'a> {
    fn load(&self, module: &str) -> starlark::Result<FrozenModule> {
        self.modules.get(module).cloned().ok_or_else(|| {
            starlark::Error::new_native(anyhow::anyhow!("Module {} not loaded statically", module))
        })
    }
}

/// Recursively load, parse and freeze a starlark dependency
#[async_recursion::async_recursion]
pub async fn eval_bzl_recursive(
    workspace: Arc<Workspace>,
    repo: Arc<Repository<'static>>,
    label: CanonicalLabel<'static>,
) -> anyhow::Result<FrozenModule> {
    let r = {
        let workspace_clone = workspace.clone();
        let repo_clone = repo.clone();
        let label_clone = label.clone();
        workspace.get_or_add_bzl(label.clone(), move || async move {
            if repo_clone.canonical_name() != label_clone.repo {
                anyhow::bail!(
                    "eval_bzl_recursive cross-repo load unimplemented for {:?}",
                    label_clone
                );
            }

            let package = label_clone.package();
            let target = label_clone.name();

            let path = if package.is_empty() {
                target.to_string()
            } else {
                format!("{}/{}", package, target)
            };

            let file = repo_clone.read_file(&path).await?;
            let mut content = String::new();
            (*file).open().await?.read_to_string(&mut content).await?;

            let loads: Vec<String> = {
                let ast = AstModule::parse(&path, content.clone(), &DIALECT_BUILD)
                    .map_err(|e| e.into_anyhow())?;
                ast.loads()
                    .into_iter()
                    .map(|l| l.module_id.to_string())
                    .collect()
            };

            let mut loaded_modules = HashMap::new();
            let mut futures: Vec<BoxFuture<'static, anyhow::Result<FrozenModule>>> = Vec::new();
            let mut module_ids = Vec::new();

            for load_str in &loads {
                let load_label = crate::bazel::label::parse_label(load_str, &label_clone)
                    .map_err(|e| anyhow::anyhow!("Failed to parse label: {:?}", e.to_string()))?;
                let canonical_load = load_label
                    .into_canonical(|r| {
                        Some(crate::bazel::label::CanonicalRepo::new(
                            r.as_str().to_string(),
                        ))
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!("Cannot resolve repo mapping for {:?}", load_str)
                    })?;

                let canonical_load_static = crate::bazel::label::CanonicalLabel::new(
                    crate::bazel::label::CanonicalRepo::new(
                        canonical_load.repo.as_str().to_string(),
                    ),
                    canonical_load.package.to_string(),
                    canonical_load.target.to_string(),
                );

                futures.push(
                    eval_bzl_recursive(
                        workspace_clone.clone(),
                        repo_clone.clone(),
                        canonical_load_static,
                    )
                    .boxed(),
                );
                module_ids.push(load_str.clone());
            }

            let results = futures::future::try_join_all(futures).await?;
            for (module_id, frozen) in module_ids.into_iter().zip(results) {
                loaded_modules.insert(module_id, frozen);
            }

            let globals = super::globals::bzl::bzl_globals_builder().build();

            let frozen_module = StarlarkModule::with_temp_heap(
                |starlark_module| -> anyhow::Result<FrozenModule> {
                    {
                        let loader = HashMapFileLoader {
                            modules: &loaded_modules,
                        };
                        let mut eval = Evaluator::new(&starlark_module);
                        eval.set_loader(&loader);
                        let ast = AstModule::parse(&path, content, &DIALECT_BUILD)
                            .map_err(|e| e.into_anyhow())?;
                        eval.eval_module(ast, &globals)
                            .map_err(|e| e.into_anyhow())?;
                    }
                    starlark_module.freeze().map_err(anyhow::Error::from)
                },
            )?;

            Ok(frozen_module)
        })
    };

    r.await.map_err(anyhow::Error::new)
}

pub async fn eval_build(
    workspace: Arc<Workspace>,
    repo: Arc<Repository<'static>>,
    path: &str, // e.g. "my_package/BUILD.bazel"
) -> anyhow::Result<HashMap<String, Rule>> {
    // 1. Construct contextual label
    let parts: Vec<&str> = path.rsplitn(2, '/').collect();
    let (package, target) = if parts.len() == 2 {
        (parts[1], parts[0])
    } else {
        ("", parts[0])
    };

    let context_label = Label::new(
        repo.canonical_name(),
        package.to_string(),
        target.to_string(),
    );

    let file = repo.read_file(path).await?;
    let mut content = String::new();
    (*file).open().await?.read_to_string(&mut content).await?;

    let loads: Vec<String> = {
        let ast =
            AstModule::parse(path, content.clone(), &DIALECT_BUILD).map_err(|e| e.into_anyhow())?;
        ast.loads()
            .into_iter()
            .map(|l| l.module_id.to_string())
            .collect()
    };

    let mut loaded_modules = HashMap::new();
    let mut futures: Vec<BoxFuture<'static, anyhow::Result<FrozenModule>>> = Vec::new();
    let mut module_ids = Vec::new();

    for load_str in &loads {
        let load_label = crate::bazel::label::parse_label(load_str, &context_label)
            .map_err(|e| anyhow::anyhow!("Failed to parse label: {:?}", e.to_string()))?;
        let canonical_load = load_label
            .into_canonical(|r| {
                // TODO: use proper apparent repo resolution from the Repository
                Some(crate::bazel::label::CanonicalRepo::new(
                    r.as_str().to_string(),
                ))
            })
            .ok_or_else(|| anyhow::anyhow!("Cannot resolve repo mapping for {:?}", load_str))?;

        let canonical_load_static = crate::bazel::label::CanonicalLabel::new(
            crate::bazel::label::CanonicalRepo::new(canonical_load.repo.as_str().to_string()),
            canonical_load.package.to_string(),
            canonical_load.target.to_string(),
        );

        futures.push(
            eval_bzl_recursive(workspace.clone(), repo.clone(), canonical_load_static).boxed(),
        );
        module_ids.push(load_str.clone());
    }

    let results = futures::future::try_join_all(futures).await?;
    for (module_id, frozen) in module_ids.into_iter().zip(results) {
        loaded_modules.insert(module_id, frozen);
    }

    let globals = super::globals::build::build_globals_builder().build();

    let extra = crate::starlark::globals::build::BuildExtra {
        rules: std::cell::RefCell::new(HashMap::new()),
    };

    StarlarkModule::with_temp_heap(|starlark_module| {
        let loader = HashMapFileLoader {
            modules: &loaded_modules,
        };
        let mut eval = Evaluator::new(&starlark_module);
        eval.set_loader(&loader);
        eval.extra = Some(&extra);

        let ast = AstModule::parse(path, content, &DIALECT_BUILD)?;
        eval.eval_module(ast, &globals)?;
        Ok::<_, starlark::Error>(())
    })
    .map_err(|e| e.into_anyhow())?;

    Ok(extra.rules.into_inner())
}
