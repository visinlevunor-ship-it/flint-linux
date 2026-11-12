#!/bin/sh
# micro, the terminal text editor with mouse support and familiar keys (static Go build).
# Usage: ports/micro/build.sh OUTDIR. Needs go (1.19 or newer) and git.
#
# Sources come from GitHub at pinned commits. Modules on GitHub are fetched with git and
# checked against micro's go.sum; the golang.org/x and gopkg.in ones are cloned from their
# GitHub mirrors at pinned commits and used in place.
. "$(dirname "$0")/../common.sh"
ports_init micro "${1:?usage: build.sh OUTDIR}"
command -v go >/dev/null 2>&1 || die "go not found"

clone() { # REPO TAG COMMIT DIR
    git -c advice.detachedHead=false clone -q --depth 1 --branch "$2" "https://github.com/$1.git" "$4" >>"$LOG" 2>&1 || die "cannot clone $1"
    got=$(git -C "$4" rev-parse HEAD)
    [ "$got" = "$3" ] || die "$1 $2 is commit $got, expected $3"
}
src="$WORK/src/micro"
clone zyedidia/micro v2.0.15 6a62575bcfdf4965f187eedafceb3400316e612b "$src"
clone golang/sys v0.30.0 863b3c4ac4975ff758815fa8d01acb6771f37177 "$src/deps/sys"
clone golang/term v0.29.0 743b2709ab25357d30ce1eac4a840eb0b7deb1bf "$src/deps/term"
clone golang/text v0.4.0 1bdb400fb39a45cc788ffe7e5d7a2a9719afc7e9 "$src/deps/text"
clone go-yaml/yaml v2.2.8 53403b58ad1b561927d19068c655246f2db79d48 "$src/deps/yaml"
say "micro"
(cd "$src" && go mod edit -replace golang.org/x/sys=./deps/sys -replace golang.org/x/term=./deps/term \
        -replace golang.org/x/text=./deps/text -replace gopkg.in/yaml.v2=./deps/yaml \
    && GOPROXY=${GOPROXY:-direct} GOPATH="$WORK/gopath" GOCACHE="$WORK/gocache" CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
       go build -trimpath -ldflags "-s -w -X github.com/zyedidia/micro/v2/internal/util.Version=2.0.15" -o "$WORK/micro" ./cmd/micro >>"$LOG" 2>&1)
chmod -R u+w "$WORK/gopath" 2>/dev/null || true
mkdir -p "$STAGE/usr/bin" "$STAGE/usr/share/licenses/micro"
cp "$WORK/micro" "$STAGE/usr/bin/micro"
cp "$src/LICENSE" "$STAGE/usr/share/licenses/micro/"
ports_package 2.0.15 "Modern terminal text editor: mouse, Ctrl-S/Ctrl-C/Ctrl-V, syntax highlighting"
