// This file declares the builtins module.
// Builtin functions will be defined in other files within this directory.

use starlark::environment::GlobalsBuilder;
use starlark::starlark_module;

#[starlark_module]
fn builtins(builder: &mut GlobalsBuilder) {}
