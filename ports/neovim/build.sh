#!/bin/sh
# Neovim (static: LuaJIT, libuv, tree-sitter and its parsers all built in).
# Usage: ports/neovim/build.sh OUTDIR. Needs cmake and git besides the common tools.
#
# Neovim's own dependency build (cmake.deps) downloads release archives from GitHub and
# checks their SHA-256. Where those downloads are blocked, DEPS_FROM_GIT=1 clones the same
# tags or commits with git instead and makes the archives from them.
. "$(dirname "$0")/../common.sh"
ports_init neovim "${1:?usage: build.sh OUTDIR}"
command -v cmake >/dev/null 2>&1 || die "cmake not found"
# LuaJIT unwinds through C frames for errors: link zig's libunwind.
export LDFLAGS="$LDFLAGS -lunwind"
src=$(unpack "$(fetch universe/n/neovim/neovim_0.12.4.orig.tar.xz 2b7b89f99b5636c5ff13b82dc4f81305c10cb55a6f58b4d1c7934b6ae188da34)")

DEPOPTS=""
if [ -n "${DEPS_FROM_GIT:-}" ]; then
    mkdir -p "$WORK/dl"
    while read -r key url; do
        case "$key" in *_URL) ;; *) continue ;; esac
        name=${key%_URL}
        case "$name" in GETTEXT|LIBICONV|LUA|WASMTIME|UNCRUSTIFY|WIN32YANK_X86_64) continue ;; esac
        case "$url" in
            https://github.com/*/archive/*)
                path=${url#https://github.com/}
                repo=$(echo "$path" | cut -d/ -f1-2)
                ref=$(echo "$path" | cut -d/ -f4- | sed 's/\.tar\.gz$//')
                d="$WORK/dl/$name"
                say "$name: $repo $ref (git)"
                git init -q "$d" && git -C "$d" fetch -q --depth 1 "https://github.com/$repo.git" "$ref" >>"$LOG" 2>&1 \
                    || git -C "$d" fetch -q --depth 1 "https://github.com/$repo.git" "refs/tags/$ref" >>"$LOG" 2>&1 \
                    || die "cannot fetch $repo $ref"
                git -C "$d" archive --prefix="$name/" FETCH_HEAD | gzip -n > "$WORK/dl/$name.tar.gz"
                DEPOPTS="$DEPOPTS -D${name}_URL=file://$WORK/dl/$name.tar.gz -D${name}_SHA256=$(sha256_of "$WORK/dl/$name.tar.gz")"
                ;;
            https://github.com/*/raw/*)
                # same file, from the raw content host (its SHA-256 still applies)
                raw=$(echo "$url" | sed 's#https://github.com/\([^/]*/[^/]*\)/raw/#https://raw.githubusercontent.com/\1/#')
                DEPOPTS="$DEPOPTS -D${name}_URL=$raw"
                ;;
        esac
    done < "$src/cmake.deps/deps.txt"
fi

# Code generation runs Lua with Neovim's nlua0 module, loaded as a shared library, which
# a static interpreter cannot do. So the generators get a host build of LuaJIT and nlua0
# (the system compiler), and the real build is a cross build for static musl.
say "host tools for code generation"
(cd "$src" && env -u CC -u CXX -u CFLAGS -u CXXFLAGS -u CPPFLAGS -u LDFLAGS -u AR -u RANLIB \
    sh -c "cmake -S cmake.deps -B '$WORK/host-deps' -G 'Unix Makefiles' -DCMAKE_BUILD_TYPE=Release \
        -DUSE_BUNDLED_GETTEXT=OFF -DUSE_BUNDLED_LIBICONV=OFF -DUSE_BUNDLED_TS_PARSERS=OFF -DUSE_BUNDLED_WASMTIME=OFF $DEPOPTS \
      && cmake --build '$WORK/host-deps' -j $JOBS \
      && cmake -S . -B '$WORK/host-build' -G 'Unix Makefiles' -DCMAKE_BUILD_TYPE=Release -DDEPS_PREFIX='$WORK/host-deps/usr' \
      && cmake --build '$WORK/host-build' --target nlua0 -j $JOBS" >>"$LOG" 2>&1)
HOSTLUA="$WORK/host-deps/usr/bin/luajit"
HOSTNLUA=$(find "$WORK/host-build" -name 'libnlua0.so' | head -n 1)
[ -x "$HOSTLUA" ] && [ -n "$HOSTNLUA" ] || die "host LuaJIT or nlua0 missing"

say "neovim dependencies"
# shellcheck disable=SC2086
(cd "$src" && cmake -S cmake.deps -B .deps -G "Unix Makefiles" -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_C_COMPILER="$CC" -DCMAKE_CXX_COMPILER="$CXX" -DCMAKE_AR="$(command -v "$AR")" -DCMAKE_RANLIB="$(command -v "$RANLIB")" \
    -DUSE_BUNDLED_GETTEXT=OFF -DUSE_BUNDLED_LIBICONV=OFF -DUSE_BUNDLED_WASMTIME=OFF $DEPOPTS >>"$LOG" 2>&1 \
    && cmake --build .deps -j "$JOBS" >>"$LOG" 2>&1)
# A static program cannot load the tree-sitter parsers (shared libraries) Neovim bundles,
# so they are compiled in: the patch makes the parser loader look there first. The .so
# files stay, because Neovim finds a parser by its file before loading it.
say "built-in tree-sitter parsers"
PARSERS="$WORK/parsers"
mkdir -p "$PARSERS"
for d in "$src"/.deps/build/src/treesitter_*/ "$src"/.deps/build/src/treesitter_markdown/tree-sitter-markdown*/; do
    [ -f "$d/src/parser.c" ] || continue
    n=$(basename "$d")
    for c in "$d"/src/parser.c "$d"/src/scanner.c; do
        [ -f "$c" ] && $CC $CFLAGS -O2 -I"$d/src" -c "$c" -o "$PARSERS/$n-$(basename "$c" .c).o" >>"$LOG" 2>&1
    done
done
$AR rcs "$PARSERS/libparsers.a" "$PARSERS"/*.o
for l in c lua vim vimdoc query markdown markdown_inline; do
    nm "$PARSERS/libparsers.a" 2>/dev/null | grep -q " T tree_sitter_$l\$" || die "parser $l was not built"
done
python3 - "$src/src/nvim/lua/treesitter.c" <<'PY' || die "cannot patch treesitter.c"
import sys
p = sys.argv[1]
s = open(p).read()
anchor = "static const TSLanguage *load_language_from_object(lua_State *L, const char *path,\n                                                   const char *lang_name, const char *symbol)\n{\n"
assert anchor in s, "load_language_from_object not found"
names = ["c", "lua", "vim", "vimdoc", "query", "markdown", "markdown_inline"]
decl = "".join(f"extern const TSLanguage *tree_sitter_{n}(void);\n" for n in names)
table = "static const struct { const char *name; const TSLanguage *(*fn)(void); } flint_builtin_parsers[] = {\n" + "".join(f'  {{ "{n}", tree_sitter_{n} }},\n' for n in names) + "};\n\n"
check = ("  for (size_t i = 0; i < sizeof(flint_builtin_parsers) / sizeof(flint_builtin_parsers[0]); i++) {\n"
         "    if (strcmp(symbol, flint_builtin_parsers[i].name) == 0) {\n"
         "      return flint_builtin_parsers[i].fn();\n"
         "    }\n"
         "  }\n")
s = s.replace(anchor, decl + table + anchor + check, 1)
open(p, "w").write(s)
PY

say "neovim"
(cd "$src" && cmake -S . -B build -G "Unix Makefiles" -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr \
    -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=x86_64 \
    -DCMAKE_C_COMPILER="$CC" -DCMAKE_AR="$(command -v "$AR")" -DCMAKE_RANLIB="$(command -v "$RANLIB")" \
    -DLUA_GEN_PRG="$HOSTLUA" -DNLUA0_HOST_PRG="$HOSTNLUA" \
    -DCMAKE_EXE_LINKER_FLAGS="-static -Wl,--gc-sections -lunwind $PARSERS/libparsers.a" \
    -DSTATIC_BUILD=ON -DENABLE_LIBINTL=OFF -DENABLE_TRANSLATIONS=OFF -DENABLE_WASMTIME=OFF >>"$LOG" 2>&1 \
    && cmake --build build -j "$JOBS" >>"$LOG" 2>&1 \
    && DESTDIR="$STAGE" cmake --install build >>"$LOG" 2>&1)
rm -rf "$STAGE/usr/share/applications" "$STAGE/usr/share/icons" "$STAGE/usr/share/man" "$STAGE/usr/share/locale"
mkdir -p "$STAGE/usr/share/licenses/neovim" && cp "$src/LICENSE.txt" "$STAGE/usr/share/licenses/neovim/"
ALLOW_DYNAMIC=1 ports_package 0.12.4 "Vim-based text editor: Lua plugins, tree-sitter highlighting, LSP"
