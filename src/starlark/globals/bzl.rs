use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::Value;
use starlark::values::none::NoneType;

pub(crate) fn bzl_globals_builder() -> GlobalsBuilder {
    let mut b = GlobalsBuilder::standard();
    bzl_globals(&mut b);
    b
}

#[starlark_module]
pub(crate) fn bzl_globals(builder: &mut GlobalsBuilder) {
    fn rule(
        #[starlark(kwargs)] _kwargs: SmallMap<&str, Value>,
        _eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        Err(starlark::Error::new_native(anyhow::anyhow!(
            "rule() unimplemented"
        )))
    }

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
