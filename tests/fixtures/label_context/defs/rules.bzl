def _implementation(ctx):
    fail("not run while loading")

context_rule = rule(
    implementation = _implementation,
    attrs = {
        "dep": attr.label(mandatory = True),
    },
)

def context_macro(name):
    context_rule(
        name = name,
        dep = Label(":from_defining_bzl.txt"),
    )
