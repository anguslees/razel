use crate::bazel::label::CanonicalLabel;
use crate::bazel::package::{PackageBuilder, PackageDefaults};
use crate::bazel::repo::Repository;
use crate::bazel::rule::{
    AttributeDependency, AttributeSchema, AttributeTransition, AttributeType, AttributeValue,
    DirectAttributeValue, RuleClass, RuleClassKind, RuleClassSpec, native_rule_class,
    native_rule_class_with_kind,
};
use crate::starlark::rule::{LabelResolver, add_context_globals, convert_attribute};
use starlark::any::ProvidesStaticType;
use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::Value;
use starlark::values::list::UnpackList;
use starlark::values::none::{NoneOr, NoneType};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::LazyLock;

#[derive(Debug, ProvidesStaticType)]
pub(crate) struct BuildExtra<'a> {
    pub builder: RefCell<PackageBuilder<'static>>,
    pub resolver: LabelResolver<'a>,
}

pub(crate) fn build_globals_builder(
    repository: &Repository<'static>,
    context: &CanonicalLabel<'_>,
) -> GlobalsBuilder {
    let mut builder = GlobalsBuilder::standard();
    add_context_globals(&mut builder, repository, context);
    build_globals(&mut builder);
    builder
}

fn attribute(
    name: &str,
    attribute_type: AttributeType,
    default: Option<DirectAttributeValue>,
    dependency: AttributeDependency,
) -> AttributeSchema {
    AttributeSchema::new(name, attribute_type, default, dependency)
}

fn label_attribute(name: &str) -> AttributeSchema {
    attribute(
        name,
        AttributeType::Label,
        None,
        AttributeDependency::Dependency,
    )
}

fn label_list_attribute(name: &str) -> AttributeSchema {
    attribute(
        name,
        AttributeType::LabelList,
        Some(DirectAttributeValue::LabelList(Vec::new())),
        AttributeDependency::Dependency,
    )
}

fn nodep_label_attribute(name: &str) -> AttributeSchema {
    attribute(
        name,
        AttributeType::Label,
        None,
        AttributeDependency::NoDependency,
    )
}

static GENRULE: LazyLock<RuleClass> = LazyLock::new(|| {
    let mut outs = attribute(
        "outs",
        AttributeType::OutputList,
        None,
        AttributeDependency::Output,
    );
    outs.mandatory = true;
    outs.allow_empty = Some(false);
    let mut cmd = attribute(
        "cmd",
        AttributeType::String,
        None,
        AttributeDependency::NoDependency,
    );
    cmd.mandatory = true;
    let mut tools = attribute(
        "tools",
        AttributeType::LabelList,
        Some(DirectAttributeValue::LabelList(Vec::new())),
        AttributeDependency::Dependency,
    );
    tools.transition = AttributeTransition::Exec;
    native_rule_class(
        "genrule",
        vec![
            attribute(
                "srcs",
                AttributeType::LabelList,
                Some(DirectAttributeValue::LabelList(Vec::new())),
                AttributeDependency::Dependency,
            ),
            tools,
            outs,
            cmd,
        ],
    )
});

static FILEGROUP: LazyLock<RuleClass> = LazyLock::new(|| {
    native_rule_class(
        "filegroup",
        ["srcs", "data"].map(label_list_attribute).into(),
    )
});

static CC_LIBRARY: LazyLock<RuleClass> = LazyLock::new(|| {
    let mut attributes: Vec<AttributeSchema> = [
        "srcs",
        "hdrs",
        "textual_hdrs",
        "deps",
        "implementation_deps",
        "data",
        "additional_compiler_inputs",
        "additional_linker_inputs",
        "module_interfaces",
    ]
    .map(label_list_attribute)
    .into();
    attributes.extend(["linkstamp", "win_def_file"].map(label_attribute));
    native_rule_class("cc_library", attributes)
});

static CC_BINARY: LazyLock<RuleClass> = LazyLock::new(|| {
    let mut attributes: Vec<AttributeSchema> = [
        "srcs",
        "deps",
        "data",
        "additional_compiler_inputs",
        "additional_linker_inputs",
        "dynamic_deps",
        "module_interfaces",
        "reexport_deps",
    ]
    .map(label_list_attribute)
    .into();
    attributes.push(label_attribute("win_def_file"));
    native_rule_class_with_kind("cc_binary", RuleClassKind::Executable, attributes)
});

static SH_BINARY: LazyLock<RuleClass> = LazyLock::new(|| {
    native_rule_class_with_kind(
        "sh_binary",
        RuleClassKind::Executable,
        ["srcs", "deps", "data"].map(label_list_attribute).into(),
    )
});

fn nonconfigurable(mut schema: AttributeSchema) -> AttributeSchema {
    schema.configurable = false;
    schema.selector_conditions_are_dependencies = false;
    schema
}

static CONFIG_SETTING: LazyLock<RuleClass> = LazyLock::new(|| {
    native_rule_class(
        "config_setting",
        vec![
            nonconfigurable(attribute(
                "values",
                AttributeType::StringDict,
                Some(DirectAttributeValue::StringDict(BTreeMap::new())),
                AttributeDependency::NoDependency,
            )),
            nonconfigurable(attribute(
                "define_values",
                AttributeType::StringDict,
                Some(DirectAttributeValue::StringDict(BTreeMap::new())),
                AttributeDependency::NoDependency,
            )),
            nonconfigurable(attribute(
                "flag_values",
                AttributeType::LabelKeyedStringDict,
                Some(DirectAttributeValue::LabelKeyedStringDict(BTreeMap::new())),
                AttributeDependency::Dependency,
            )),
            nonconfigurable(label_list_attribute("constraint_values")),
        ],
    )
});

static CONSTRAINT_SETTING: LazyLock<RuleClass> = LazyLock::new(|| {
    native_rule_class(
        "constraint_setting",
        vec![
            nonconfigurable(nodep_label_attribute("default_constraint_value")),
            nonconfigurable(nodep_label_attribute("refines_constraint_value")),
        ],
    )
});

static CONSTRAINT_VALUE: LazyLock<RuleClass> = LazyLock::new(|| {
    let mut constraint_setting = label_attribute("constraint_setting");
    constraint_setting.mandatory = true;
    constraint_setting.configurable = false;
    constraint_setting.selector_conditions_are_dependencies = false;
    native_rule_class("constraint_value", vec![constraint_setting])
});

static PLATFORM: LazyLock<RuleClass> = LazyLock::new(|| {
    native_rule_class(
        "platform",
        vec![
            nonconfigurable(label_list_attribute("constraint_values")),
            nonconfigurable(label_list_attribute("parents")),
            nonconfigurable(attribute(
                "flags",
                AttributeType::StringList,
                Some(DirectAttributeValue::StringList(Vec::new())),
                AttributeDependency::NoDependency,
            )),
        ],
    )
});

static ALIAS: LazyLock<RuleClass> = LazyLock::new(|| {
    let mut actual = label_attribute("actual");
    actual.mandatory = true;
    native_rule_class("alias", vec![actual])
});

fn instantiate_native<'v>(
    name: &str,
    kwargs: SmallMap<&str, Value<'v>>,
    eval: &mut Evaluator<'v, '_, '_>,
    rule_class: &'static RuleClass,
) -> starlark::Result<NoneType> {
    let extra = eval
        .extra
        .and_then(|extra| extra.downcast_ref::<BuildExtra<'_>>())
        .ok_or_else(|| starlark_error("native rule called outside BUILD evaluation"))?;
    let mut explicit = BTreeMap::from([(
        "name",
        AttributeValue::direct(DirectAttributeValue::String(name.to_owned())),
    )]);
    for (name, value) in kwargs {
        if let Some(schema) = rule_class.attribute(name) {
            explicit.insert(name, convert_attribute(schema, value, &extra.resolver)?);
        }
    }
    extra
        .builder
        .borrow_mut()
        .instantiate_rule(RuleClassSpec::Native(rule_class), explicit)
        .map_err(starlark_error)?;
    Ok(NoneType)
}

fn package_value<'v>(
    name: &str,
    attribute_type: AttributeType,
    value: Value<'v>,
    resolver: &LabelResolver,
) -> starlark::Result<DirectAttributeValue> {
    let schema = AttributeSchema::new(
        name,
        attribute_type,
        None,
        AttributeDependency::NoDependency,
    );
    convert_attribute(&schema, value, resolver)?
        .into_direct_value()
        .ok_or_else(|| starlark_error(format!("package argument `{name}` cannot use select()")))
}

#[starlark_module]
pub(crate) fn build_globals(builder: &mut GlobalsBuilder) {
    fn rule(
        #[starlark(kwargs)] _kwargs: SmallMap<&str, Value>,
        _eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        Err(starlark_error("rule() is not allowed in BUILD files"))
    }

    fn genrule<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        instantiate_native(name, kwargs, eval, &GENRULE)
    }

    fn cc_library<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        instantiate_native(name, kwargs, eval, &CC_LIBRARY)
    }

    fn cc_binary<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        instantiate_native(name, kwargs, eval, &CC_BINARY)
    }

    fn filegroup<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        instantiate_native(name, kwargs, eval, &FILEGROUP)
    }

    fn sh_binary<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        instantiate_native(name, kwargs, eval, &SH_BINARY)
    }

    fn config_setting<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        instantiate_native(name, kwargs, eval, &CONFIG_SETTING)
    }

    fn constraint_setting<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        instantiate_native(name, kwargs, eval, &CONSTRAINT_SETTING)
    }

    fn constraint_value<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        instantiate_native(name, kwargs, eval, &CONSTRAINT_VALUE)
    }

    fn platform<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        instantiate_native(name, kwargs, eval, &PLATFORM)
    }

    fn alias<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        instantiate_native(name, kwargs, eval, &ALIAS)
    }

    fn exports_files<'v>(
        files: UnpackList<&str>,
        #[starlark(default = NoneOr::None)] visibility: NoneOr<Value<'v>>,
        #[starlark(default = NoneOr::None)] licenses: NoneOr<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        if !matches!(licenses, NoneOr::None) {
            return Err(starlark_error(
                "exports_files(licenses=...) is not supported",
            ));
        }
        let extra = eval
            .extra
            .and_then(|extra| extra.downcast_ref::<BuildExtra<'_>>())
            .ok_or_else(|| starlark_error("exports_files() called outside BUILD evaluation"))?;
        let visibility: Vec<CanonicalLabel<'static>> = match visibility {
            NoneOr::None => Vec::new(),
            NoneOr::Other(value) => match package_value(
                "visibility",
                AttributeType::LabelList,
                value,
                &extra.resolver,
            )? {
                DirectAttributeValue::LabelList(labels) => labels,
                _ => unreachable!(),
            },
        };
        let mut builder = extra.builder.borrow_mut();
        for file in files.items {
            builder
                .export_source_file(file, visibility.clone())
                .map_err(starlark_error)?;
        }
        Ok(NoneType)
    }

    fn package<'v>(
        #[starlark(kwargs)] kwargs: SmallMap<&str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<NoneType> {
        let extra = eval
            .extra
            .and_then(|extra| extra.downcast_ref::<BuildExtra<'_>>())
            .ok_or_else(|| starlark_error("package() called outside BUILD evaluation"))?;
        let mut defaults = PackageDefaults::default();
        for (name, value) in kwargs {
            match name {
                "default_visibility" => {
                    let DirectAttributeValue::LabelList(labels) =
                        package_value(name, AttributeType::LabelList, value, &extra.resolver)?
                    else {
                        unreachable!()
                    };
                    defaults.visibility = labels;
                }
                "default_testonly" => {
                    let DirectAttributeValue::Bool(value) =
                        package_value(name, AttributeType::Bool, value, &extra.resolver)?
                    else {
                        unreachable!()
                    };
                    defaults.testonly = value;
                }
                "default_deprecation" => {
                    let DirectAttributeValue::String(value) =
                        package_value(name, AttributeType::String, value, &extra.resolver)?
                    else {
                        unreachable!()
                    };
                    defaults.deprecation = value;
                }
                "default_package_metadata" => {
                    let DirectAttributeValue::LabelList(labels) =
                        package_value(name, AttributeType::LabelList, value, &extra.resolver)?
                    else {
                        unreachable!()
                    };
                    defaults.package_metadata = labels;
                }
                "features" => {
                    let DirectAttributeValue::StringList(features) =
                        package_value(name, AttributeType::StringList, value, &extra.resolver)?
                    else {
                        unreachable!()
                    };
                    defaults.features = features;
                }
                "default_applicable_licenses" | "licenses" if value.is_none() => {}
                "default_applicable_licenses" | "licenses" => {
                    return Err(starlark_error(format!(
                        "non-default package argument `{name}` is not supported"
                    )));
                }
                _ => {
                    return Err(starlark_error(format!(
                        "unsupported package argument `{name}`"
                    )));
                }
            }
        }
        extra
            .builder
            .borrow_mut()
            .set_package_defaults(defaults)
            .map_err(starlark_error)?;
        Ok(NoneType)
    }
}

fn starlark_error(error: impl std::fmt::Display) -> starlark::Error {
    starlark::Error::new_native(anyhow::anyhow!(error.to_string()))
}
