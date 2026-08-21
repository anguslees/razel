def _implementation(ctx):
    pass

mandatory_rule = rule(
    implementation = _implementation,
    attrs = {
        "required": attr.string(default = "still required", mandatory = True),
    },
)
