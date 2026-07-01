use crate::bazel::rule::Rule;
use starlark::any::ProvidesStaticType;
use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::Value;
use starlark::values::none::NoneType;
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Debug, ProvidesStaticType)]
pub(crate) struct BuildExtra {
    pub rules: RefCell<HashMap<String, Rule>>,
}

pub(crate) fn build_globals_builder() -> GlobalsBuilder {
    let mut b = GlobalsBuilder::standard();
    build_globals(&mut b);
    b
}

#[starlark_module]
pub(crate) fn build_globals(builder: &mut GlobalsBuilder) {
    fn rule(
        #[starlark(kwargs)] _kwargs: SmallMap<&str, Value>,
        _eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        Err(starlark::Error::new_native(anyhow::anyhow!(
            "rule() not allowed in BUILD files"
        )))
    }

    fn genrule(
        #[starlark(require = named)] name: &str,
        #[starlark(kwargs)] _kwargs: SmallMap<&str, Value>,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        if let Some(extra) = eval
            .extra
            .as_ref()
            .and_then(|e| e.downcast_ref::<BuildExtra>())
        {
            extra.rules.borrow_mut().insert(
                name.to_string(),
                Rule {
                    name: name.to_string(),
                    rule_class: "genrule".to_string(),
                },
            );
        }
        Ok(NoneType)
    }

    fn cc_library(
        #[starlark(require = named)] name: &str,
        #[starlark(kwargs)] _kwargs: SmallMap<&str, Value>,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        if let Some(extra) = eval
            .extra
            .as_ref()
            .and_then(|e| e.downcast_ref::<BuildExtra>())
        {
            extra.rules.borrow_mut().insert(
                name.to_string(),
                Rule {
                    name: name.to_string(),
                    rule_class: "cc_library".to_string(),
                },
            );
        }
        Ok(NoneType)
    }

    fn cc_binary(
        #[starlark(require = named)] name: &str,
        #[starlark(kwargs)] _kwargs: SmallMap<&str, Value>,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        if let Some(extra) = eval
            .extra
            .as_ref()
            .and_then(|e| e.downcast_ref::<BuildExtra>())
        {
            extra.rules.borrow_mut().insert(
                name.to_string(),
                Rule {
                    name: name.to_string(),
                    rule_class: "cc_binary".to_string(),
                },
            );
        }
        Ok(NoneType)
    }

    fn filegroup(
        #[starlark(require = named)] name: &str,
        #[starlark(kwargs)] _kwargs: SmallMap<&str, Value>,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        if let Some(extra) = eval
            .extra
            .as_ref()
            .and_then(|e| e.downcast_ref::<BuildExtra>())
        {
            extra.rules.borrow_mut().insert(
                name.to_string(),
                Rule {
                    name: name.to_string(),
                    rule_class: "filegroup".to_string(),
                },
            );
        }
        Ok(NoneType)
    }

    fn sh_binary(
        #[starlark(require = named)] name: &str,
        #[starlark(kwargs)] _kwargs: SmallMap<&str, Value>,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        if let Some(extra) = eval
            .extra
            .as_ref()
            .and_then(|e| e.downcast_ref::<BuildExtra>())
        {
            extra.rules.borrow_mut().insert(
                name.to_string(),
                Rule {
                    name: name.to_string(),
                    rule_class: "sh_binary".to_string(),
                },
            );
        }
        Ok(NoneType)
    }
}
