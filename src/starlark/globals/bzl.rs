use crate::bazel::label::CanonicalLabel;
use crate::bazel::repo::Repository;
use crate::starlark::rule::{add_attr_namespace, add_context_globals, bzl_rule_globals};
use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::Value;
use starlark::values::none::NoneType;

pub(crate) fn bzl_globals_builder(
    repository: &Repository<'static>,
    context: &CanonicalLabel<'_>,
) -> GlobalsBuilder {
    let mut builder = GlobalsBuilder::standard();
    add_context_globals(&mut builder, repository, context);
    add_attr_namespace(&mut builder);
    bzl_rule_globals(&mut builder);
    unsupported_globals(&mut builder);
    builder
}

#[starlark_module]
fn unsupported_globals(builder: &mut GlobalsBuilder) {
    fn provider(
        #[starlark(kwargs)] _kwargs: SmallMap<&str, Value>,
        _eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        Err(starlark::Error::new_native(anyhow::anyhow!(
            "provider() unimplemented"
        )))
    }

    fn aspect(
        #[starlark(kwargs)] _kwargs: SmallMap<&str, Value>,
        _eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        Err(starlark::Error::new_native(anyhow::anyhow!(
            "aspect() unimplemented"
        )))
    }

    fn repository_rule(
        #[starlark(kwargs)] _kwargs: SmallMap<&str, Value>,
        _eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        Err(starlark::Error::new_native(anyhow::anyhow!(
            "repository_rule() unimplemented"
        )))
    }
}
