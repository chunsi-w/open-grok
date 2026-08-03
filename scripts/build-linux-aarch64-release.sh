#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
readonly REPO_ROOT
readonly VERSION_FILE="${REPO_ROOT}/OPEN_GROK_VERSION"
readonly DIST_DIR="${REPO_ROOT}/dist"
readonly TARGET_TRIPLE="aarch64-unknown-linux-gnu"
readonly PACKAGE_NAME="open-grok-linux-aarch64"
readonly ARCHIVE_PATH="${DIST_DIR}/${PACKAGE_NAME}.tar.gz"
readonly ARCHIVE_CHECKSUM_PATH="${ARCHIVE_PATH}.sha256"
readonly EXPECTED_PROTOC_VERSION="libprotoc 29.3"
readonly EXPECTED_RG_VERSION="ripgrep 15.0.0"
readonly TARGET_RUSTFLAGS="-C target-cpu=generic -C force-unwind-tables=yes -C link-arg=-Wl,-z,relro,-z,now,-z,noexecstack"

STAGE_ROOT=""
ARCHIVE_TMP=""
CHECKSUM_TMP=""

fail() {
    echo "Error: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

cleanup() {
    if [[ -n "$STAGE_ROOT" && "$STAGE_ROOT" == "$DIST_DIR"/.linux-aarch64-stage.* ]]; then
        rm -rf -- "$STAGE_ROOT"
    fi
    if [[ -n "$ARCHIVE_TMP" && "$ARCHIVE_TMP" == "$DIST_DIR"/.open-grok-linux-aarch64.tar.gz.tmp.* ]]; then
        rm -f -- "$ARCHIVE_TMP"
    fi
    if [[ -n "$CHECKSUM_TMP" && "$CHECKSUM_TMP" == "$DIST_DIR"/.open-grok-linux-aarch64.tar.gz.sha256.tmp.* ]]; then
        rm -f -- "$CHECKSUM_TMP"
    fi
}

verify_elf() {
    local binary="$1"
    local header program_headers stack dynamic ldd_output

    header="$(readelf -h "$binary")"
    grep -Eq 'Class:[[:space:]]+ELF64' <<<"$header" || fail "release artifact is not ELF64"
    grep -Eq 'Data:.*little endian' <<<"$header" || fail "release artifact is not little-endian"
    grep -Eq 'Type:[[:space:]]+DYN' <<<"$header" || fail "release artifact is not PIE"
    grep -Eq 'Machine:[[:space:]]+AArch64' <<<"$header" || fail "release artifact is not AArch64"

    program_headers="$(readelf -W -l "$binary")"
    stack="$(awk '$1 == "GNU_STACK" { print; exit }' <<<"$program_headers")"
    [[ -n "$stack" ]] || fail "release artifact has no GNU_STACK program header"
    [[ "$stack" != *RWE* ]] || fail "release artifact requests an executable stack"
    grep -q 'GNU_RELRO' <<<"$program_headers" || fail "release artifact has no GNU_RELRO"

    dynamic="$(readelf -W -d "$binary")"
    grep -Eq '\(BIND_NOW\)|FLAGS.*NOW' <<<"$dynamic" || fail "release artifact lacks BIND_NOW"
    ldd_output="$(ldd "$binary")"
    [[ "$ldd_output" != *'not found'* ]] || fail "release artifact has unresolved libraries: ${ldd_output}"
}

verify_tool_inputs() {
    local tools_rg shell_rg

    [[ -n "${PROTOC:-}" && -x "$PROTOC" ]] || fail "PROTOC must point to an executable"
    [[ "$("$PROTOC" --version)" == "$EXPECTED_PROTOC_VERSION" ]] ||
        fail "release builds require ${EXPECTED_PROTOC_VERSION}"

    tools_rg="${GROK_TOOLS_BUNDLE_RG_PATH:-}"
    shell_rg="${GROK_SHELL_BUNDLE_RG_PATH:-}"
    [[ -n "$tools_rg" && -x "$tools_rg" ]] || fail "GROK_TOOLS_BUNDLE_RG_PATH is required"
    [[ -n "$shell_rg" && -x "$shell_rg" ]] || fail "GROK_SHELL_BUNDLE_RG_PATH is required"
    cmp -s "$tools_rg" "$shell_rg" || fail "tools and shell must bundle the same ripgrep binary"
    [[ "$("$tools_rg" --version | sed -n '1p')" == "$EXPECTED_RG_VERSION" ]] ||
        fail "release builds require ${EXPECTED_RG_VERSION}"
    grep -Eq 'Machine:[[:space:]]+AArch64' <<<"$(readelf -h "$tools_rg")" ||
        fail "bundled ripgrep is not AArch64"
}

write_build_info() {
    local binary="$1"
    local output="$2"
    local version="$3"
    local max_glibc max_glibcxx

    max_glibc="$(
        readelf --version-info "$binary" |
            sed -nE 's/.*Name: (GLIBC_[0-9.]+).*/\1/p' |
            sort -Vu |
            tail -n 1
    )"
    max_glibcxx="$(
        readelf --version-info "$binary" |
            sed -nE 's/.*Name: (GLIBCXX_[0-9.]+).*/\1/p' |
            sort -Vu |
            tail -n 1
    )"

    {
        printf 'version=%s\n' "$version"
        printf 'source_commit=%s\n' "$(git -C "$REPO_ROOT" rev-parse HEAD)"
        printf 'source_date_epoch=%s\n' "$SOURCE_DATE_EPOCH"
        printf 'target=%s\n' "$TARGET_TRIPLE"
        printf 'target_cpu=generic\n'
        printf 'jemalloc_page_size=4096\n'
        printf 'rustc=%s\n' "$(rustc --version)"
        printf 'cargo=%s\n' "$(cargo --version)"
        printf 'builder_kernel=%s\n' "$(uname -r)"
        printf 'builder_glibc=%s\n' "$(ldd --version | sed -n '1p')"
        printf 'binary_file=%s\n' "$(file "$binary")"
        printf 'max_required_glibc=%s\n' "${max_glibc:-none}"
        printf 'max_required_glibcxx=%s\n' "${max_glibcxx:-none}"
        printf 'binary_sha256=%s\n' "$(sha256sum "$binary" | awk '{ print $1 }')"
        printf '\nDynamic libraries:\n'
        ldd "$binary"
    } >"$output"
}

main() {
    local version commit source_binary package_dir staged_binary
    local version_output binary_checksum

    [[ "$(uname -s)" == "Linux" ]] || fail "this builder requires Linux"
    [[ "$(uname -m)" == "aarch64" || "$(uname -m)" == "arm64" ]] ||
        fail "this builder requires a native AArch64 host"
    [[ "$(getconf PAGESIZE)" == "4096" ]] || fail "this artifact is intentionally built for 4 KiB pages"

    for command in cargo cmp file git gzip ldd readelf rustc sed sha256sum sort strip tar; do
        require_command "$command"
    done
    verify_tool_inputs

    [[ -f "$VERSION_FILE" ]] || fail "missing ${VERSION_FILE}"
    version="$(sed -n '1p' "$VERSION_FILE" | tr -d '\r')"
    [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+([.-][0-9A-Za-z]+)*)?$ ]] ||
        fail "invalid Open Grok version '${version}'"
    [[ -z "$(git -C "$REPO_ROOT" status --porcelain --untracked-files=normal)" ]] ||
        fail "release builds require a clean git worktree"

    commit="$(git -C "$REPO_ROOT" rev-parse --short HEAD)"
    export GROK_VERSION="$version"
    SOURCE_DATE_EPOCH="$(git -C "$REPO_ROOT" show -s --format=%ct HEAD)"
    export SOURCE_DATE_EPOCH
    export CARGO_INCREMENTAL=0
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="$TARGET_RUSTFLAGS"
    export AARCH64_UNKNOWN_LINUX_GNU_JEMALLOC_SYS_WITH_LG_PAGE=12
    export LC_ALL=C
    export TZ=UTC

    cd "$REPO_ROOT"
    cargo clean --quiet --profile release-dist --target "$TARGET_TRIPLE" \
        -p xai-grok-pager-bin \
        -p xai-grok-pager \
        -p xai-grok-shell \
        -p xai-grok-tools \
        -p xai-grok-version
    cargo build \
        --locked \
        --profile release-dist \
        --features release-dist \
        --target "$TARGET_TRIPLE" \
        -p xai-grok-pager-bin \
        --bin open-grok

    source_binary="${REPO_ROOT}/target/${TARGET_TRIPLE}/release-dist/open-grok"
    [[ -x "$source_binary" ]] || fail "Cargo did not produce ${source_binary}"

    mkdir -p "$DIST_DIR"
    STAGE_ROOT="$(mktemp -d "${DIST_DIR}/.linux-aarch64-stage.XXXXXX")"
    trap cleanup EXIT
    package_dir="${STAGE_ROOT}/${PACKAGE_NAME}"
    mkdir -p "$package_dir"
    staged_binary="${package_dir}/open-grok"
    cp "$source_binary" "$staged_binary"
    chmod 0755 "$staged_binary"
    strip --strip-unneeded "$staged_binary"
    verify_elf "$staged_binary"

    mkdir -p "${STAGE_ROOT}/smoke-home" "${STAGE_ROOT}/smoke-user"
    version_output="$(
        HOME="${STAGE_ROOT}/smoke-user" \
            OPENGROK_HOME="${STAGE_ROOT}/smoke-home" \
            OPENGROK_DISABLE_AUTOUPDATER=1 \
            "$staged_binary" --version
    )"
    [[ "$version_output" == *"$version"* ]] || fail "binary did not report version ${version}"
    [[ "$version_output" == *"$commit"* ]] || fail "binary did not report commit ${commit}"
    HOME="${STAGE_ROOT}/smoke-user" \
        OPENGROK_HOME="${STAGE_ROOT}/smoke-home" \
        OPENGROK_DISABLE_AUTOUPDATER=1 \
        "$staged_binary" --help >"${STAGE_ROOT}/help.txt"
    [[ -s "${STAGE_ROOT}/help.txt" ]] || fail "binary --help smoke produced no output"

    binary_checksum="$(sha256sum "$staged_binary" | awk '{ print $1 }')"
    printf '%s  open-grok\n' "$binary_checksum" >"${package_dir}/open-grok.sha256"
    write_build_info "$staged_binary" "${package_dir}/build-info.txt" "$version"
    cp LICENSE THIRD-PARTY-NOTICES "$package_dir/"
    chmod 0644 "${package_dir}/open-grok.sha256" "${package_dir}/build-info.txt" \
        "${package_dir}/LICENSE" "${package_dir}/THIRD-PARTY-NOTICES"

    ARCHIVE_TMP="${DIST_DIR}/.${PACKAGE_NAME}.tar.gz.tmp.$$"
    CHECKSUM_TMP="${DIST_DIR}/.${PACKAGE_NAME}.tar.gz.sha256.tmp.$$"
    tar \
        --sort=name \
        --mtime="@${SOURCE_DATE_EPOCH}" \
        --owner=0 \
        --group=0 \
        --numeric-owner \
        --format=gnu \
        -C "$STAGE_ROOT" \
        -cf - "$PACKAGE_NAME" | gzip -n -6 >"$ARCHIVE_TMP"
    printf '%s  %s.tar.gz\n' "$(sha256sum "$ARCHIVE_TMP" | awk '{ print $1 }')" \
        "$PACKAGE_NAME" >"$CHECKSUM_TMP"
    mv -f "$ARCHIVE_TMP" "$ARCHIVE_PATH"
    ARCHIVE_TMP=""
    mv -f "$CHECKSUM_TMP" "$ARCHIVE_CHECKSUM_PATH"
    CHECKSUM_TMP=""

    echo "Release assets:" >&2
    echo "  ${ARCHIVE_PATH}" >&2
    echo "  ${ARCHIVE_CHECKSUM_PATH}" >&2
}

main "$@"
