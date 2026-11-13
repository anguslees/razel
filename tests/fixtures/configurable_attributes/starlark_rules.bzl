def _implementation(ctx):
    fail("rule implementations are analysis-time only")

# Starlark rules exported under built-in rule names must not receive built-in semantics.
alias = rule(implementation = _implementation)

constraint_value = rule(implementation = _implementation)

# An optional label has no default, so a select() branch of None leaves it unset.
optional_label = rule(
    implementation = _implementation,
    attrs = {
        "dep": attr.label(),
        "required": attr.label(mandatory = True),
    },
)
