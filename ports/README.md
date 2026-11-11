# Ports

Recipes that build third-party programs into `pkg` package sources. Each directory has a
`control` file and a `build.sh OUTDIR` that downloads a pinned upstream release, builds it
(statically, since Flint has no shared libraries) and stages `OUTDIR/control` and
`OUTDIR/files/`.

```sh
ports/fastfetch/build.sh /tmp/fastfetch      # needs a Linux host with musl-tools, cmake, ninja
pkg build /tmp/fastfetch repo/x86_64/
pkg index repo/x86_64/
pkg sign repo/x86_64/ ~/keys/flint.key
```

Then publish `repo/` (for example as a GitHub repository; pkg reads it over HTTPS).
