def _implementation(ctx):
    pass

bad_name = rule(
    implementation = _implementation,
    test = True,
)
