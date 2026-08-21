def _implementation(ctx):
    fail("loading queries must not invoke rule implementations")

inventory_rule = rule(
    implementation = _implementation,
    attrs = {
        "srcs": attr.label_list(allow_files = False),
        "out": attr.output(mandatory = True),
        "message": attr.string(default = "default message"),
        "_tool": attr.label(default = "//:private_tool"),
    },
)
