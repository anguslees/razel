use crate::bazel::label::{CanonicalLabel, Label};
use crate::bazel::package::{
    BoxFileStore, File, Package, PackageBuilder, PackageId, PackageSource,
};
use crate::bazel::repo::Repository;
use crate::bazel::rule::RuleClassId;
use crate::starlark::rule::{BzlExtra, LabelResolver, resolve_starlark_rule_class};
use crate::workspace::Workspace;
use futures::future::{BoxFuture, FutureExt};
use starlark::environment::{FrozenModule, Module as StarlarkModule};
use starlark::eval::{Evaluator, FileLoader};
use starlark::syntax::{AstModule, Dialect};
use starlark::values::OwnedFrozenValue;
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

impl FileLoader for HashMapFileLoader<'_> {
    fn load(&self, module: &str) -> starlark::Result<FrozenModule> {
        self.modules.get(module).cloned().ok_or_else(|| {
            starlark::Error::new_native(anyhow::anyhow!("Module {module} not loaded statically"))
        })
    }
}

#[async_recursion::async_recursion]
pub async fn eval_bzl_recursive(
    workspace: Arc<Workspace>,
    repo: Arc<Repository<'static>>,
    label: CanonicalLabel<'static>,
) -> anyhow::Result<FrozenModule> {
    let future = {
        let workspace_clone = workspace.clone();
        let repo_clone = repo.clone();
        let label_clone = label.clone();
        workspace.get_or_add_bzl(label, move || async move {
            if repo_clone.canonical_name() != &label_clone.repo {
                anyhow::bail!(
                    "eval_bzl_recursive cross-repo load unimplemented for {label_clone:?}"
                );
            }

            let path = repository_path(label_clone.package(), label_clone.name());
            let file = repo_clone.read_file(&path).await?;
            let mut content = String::new();
            (*file).open().await?.read_to_string(&mut content).await?;

            let ast = AstModule::parse(&path, content, &DIALECT_BUILD)
                .map_err(starlark::Error::into_anyhow)?;
            let mut loaded_modules = HashMap::new();
            let mut futures: Vec<BoxFuture<'static, anyhow::Result<FrozenModule>>> = Vec::new();
            let mut module_ids = Vec::new();
            for load in ast.loads() {
                let load_str = load.module_id;
                let load_label = crate::bazel::label::parse_label(load_str, &label_clone)
                    .map_err(|error| anyhow::anyhow!("Failed to parse label: {error}"))?;
                let canonical_load = repo_clone.resolve_label(load_label).ok_or_else(|| {
                    anyhow::anyhow!("Cannot resolve repo mapping for {load_str:?}")
                })?;
                futures.push(
                    eval_bzl_recursive(
                        workspace_clone.clone(),
                        repo_clone.clone(),
                        canonical_load.into_owned(),
                    )
                    .boxed(),
                );
                // eval_module consumes the AST, so loader keys cannot borrow from it.
                module_ids.push(load_str.to_owned());
            }
            let results = futures::future::try_join_all(futures).await?;
            for (module_id, frozen) in module_ids.into_iter().zip(results) {
                loaded_modules.insert(module_id, frozen);
            }

            let globals =
                super::globals::bzl::bzl_globals_builder(&repo_clone, &label_clone).build();
            let resolver = LabelResolver::new(&repo_clone, label_clone.as_borrowed());
            let extra = BzlExtra::new(resolver);
            StarlarkModule::with_temp_heap(|starlark_module| -> anyhow::Result<FrozenModule> {
                {
                    let loader = HashMapFileLoader {
                        modules: &loaded_modules,
                    };
                    let mut eval = Evaluator::new(&starlark_module);
                    eval.set_loader(&loader);
                    eval.extra = Some(&extra);
                    eval.eval_module(ast, &globals)
                        .map_err(|error| error.into_anyhow())?;
                }
                starlark_module.freeze().map_err(anyhow::Error::from)
            })
        })
    };
    future.await.map_err(anyhow::Error::new)
}

pub async fn eval_build(
    workspace: Arc<Workspace>,
    repo: Arc<Repository<'static>>,
    package_id: PackageId<'static>,
    source: &PackageSource<BoxFileStore<'static>>,
) -> anyhow::Result<Package> {
    let path = repository_path(source.path(), source.build_file_name());
    let context_label = Label::new(
        repo.canonical_name().as_borrowed(),
        source.path(),
        source.build_file_name(),
    );

    let mut content = String::new();
    (**source.build_file())
        .open()
        .await?
        .read_to_string(&mut content)
        .await?;
    let ast =
        AstModule::parse(&path, content, &DIALECT_BUILD).map_err(starlark::Error::into_anyhow)?;
    let mut loaded_modules = HashMap::new();
    let mut futures: Vec<BoxFuture<'static, anyhow::Result<FrozenModule>>> = Vec::new();
    let mut module_ids = Vec::new();
    for load in ast.loads() {
        let load_str = load.module_id;
        let load_label = crate::bazel::label::parse_label(load_str, &context_label)
            .map_err(|error| anyhow::anyhow!("Failed to parse label: {error}"))?;
        let canonical_load = repo
            .resolve_label(load_label)
            .ok_or_else(|| anyhow::anyhow!("Cannot resolve repo mapping for {load_str:?}"))?;
        futures.push(
            eval_bzl_recursive(workspace.clone(), repo.clone(), canonical_load.into_owned())
                .boxed(),
        );
        // eval_module consumes the AST, so loader keys cannot borrow from it.
        module_ids.push(load_str.to_owned());
    }
    let results = futures::future::try_join_all(futures).await?;
    for (module_id, frozen) in module_ids.into_iter().zip(results) {
        loaded_modules.insert(module_id, frozen);
    }

    let globals = super::globals::build::build_globals_builder(&repo, &context_label).build();
    let resolver = LabelResolver::new(&repo, context_label);
    let extra = crate::starlark::globals::build::BuildExtra {
        builder: std::cell::RefCell::new(PackageBuilder::new(
            package_id,
            source.build_file_name(),
        )?),
        resolver,
    };

    StarlarkModule::with_temp_heap(|starlark_module| {
        let loader = HashMapFileLoader {
            modules: &loaded_modules,
        };
        let mut eval = Evaluator::new(&starlark_module);
        eval.set_loader(&loader);
        eval.extra = Some(&extra);
        eval.eval_module(ast, &globals)?;
        Ok::<_, starlark::Error>(())
    })
    .map_err(starlark::Error::into_anyhow)?;

    let builder = extra.builder.into_inner();
    let mut rule_classes = HashMap::<RuleClassId, OwnedFrozenValue>::new();
    for id in builder.starlark_rule_classes() {
        let RuleClassId::Starlark { defining_bzl, .. } = id else {
            continue;
        };
        let module =
            eval_bzl_recursive(workspace.clone(), repo.clone(), defining_bzl.clone()).await?;
        let rule_class = resolve_starlark_rule_class(&module, id)?;
        rule_classes.insert(id.clone(), rule_class);
    }
    builder.finish(&rule_classes)
}

fn repository_path(package: &str, file: &str) -> String {
    if package.is_empty() {
        file.to_owned()
    } else {
        format!("{package}/{file}")
    }
}
