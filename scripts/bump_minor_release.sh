#!/usr/bin/env sh
# Bump every h12tiny workspace crate to its next minor version and publish
# the workspace in dependency order.
#
# Each crate moves to minor(max(local, registry)) + 1 with the patch reset.
# A registry that ran ahead is honored as the base; a local version ahead
# of the registry without a matching release commit fails instead of
# guessing. Unpublished crates join the release at minor(local) + 1.
# Inter-crate requirements track their dependency's new version; anything
# unexpected fails loudly before anything is edited.
# One commit carries the release and each crate gets a <name>-v<version> tag.
# Re-running after a partial publish finishes the remaining crates instead
# of failing.

set -eu
LC_ALL=C
export LC_ALL

fail() {
    printf '%s\n' "make bump: $*" >&2
    exit 1
}

SCRIPT_DIR=$(CDPATH= cd -P "$(dirname "$0")" && pwd -P) || fail "cannot locate script directory"
ROOT=$(CDPATH= cd -P "$SCRIPT_DIR/.." && pwd -P) || fail "cannot locate repository root"
cd "$ROOT"

for command_name in awk cargo cp dirname git grep mkdir mv rm sed sort; do
    command -v "$command_name" >/dev/null 2>&1 || fail "required command not found: $command_name"
done

temporary_manifest=
backup_dir=
release_started=0
release_committed=0
release_subject="release: h12tiny workspace minor"
newline=$(printf '\nX')
newline=${newline%X}

on_exit() {
    exit_status=$1
    trap - 0 HUP INT TERM

    if [ "$release_started" -eq 1 ] && [ "$release_committed" -eq 0 ]; then
        if head_subject=$(git log -1 --format=%s 2>/dev/null); then
            :
        else
            head_subject=
        fi
        if [ "$head_subject" != "$release_subject" ]; then
            # RESET_PATHS is derived from manifest paths, which may not
            # contain whitespace (rejected at discovery).
            # shellcheck disable=SC2086
            if ! git reset -- $RESET_PATHS; then
                printf '%s\n' "make bump: failed to unstage release inputs; backups remain in $backup_dir" >&2
                exit 1
            fi
            restore_failed=0
            for name in $PKG_SPACE; do
                file=$(manifest_for "$name") || restore_failed=1
                if ! cp -p "$backup_dir/$name.toml" "$ROOT/$file"; then
                    restore_failed=1
                fi
            done
            if ! cp -p "$backup_dir/Cargo.lock" "$ROOT/Cargo.lock"; then
                restore_failed=1
            fi
            if [ "$restore_failed" -ne 0 ]; then
                printf '%s\n' "make bump: failed to restore release inputs; backups remain in $backup_dir" >&2
                exit 1
            fi
        fi
    fi

    if [ -n "$temporary_manifest" ]; then
        rm -f "$temporary_manifest" || exit_status=1
    fi
    if [ -n "$backup_dir" ]; then
        rm -rf "$backup_dir" || exit_status=1
    fi
    exit "$exit_status"
}

trap 'on_exit $?' 0
trap 'exit 1' HUP INT TERM

read_package_metadata() {
    awk '
        /^[ \t]*\[package\][ \t]*(#.*)?$/ { section = "package"; next }
        /^[ \t]*\[/ { section = ""; next }
        section == "package" {
            line = $0
            sub(/^[ \t]*/, "", line)
            key = line
            sub(/[ \t]*=.*/, "", key)
            if (key != "name" && key != "version") next

            value = line
            sub(/^[^=]*=[ \t]*/, "", value)
            if (key == "name") {
                name_count++
                if (value !~ /^"[-A-Za-z0-9_]+"([ \t]*#.*)?$/) invalid = 1
                sub(/^"/, "", value)
                sub(/".*/, "", value)
                package_name = value
            } else {
                version_count++
                if (value !~ /^"(0|[1-9][0-9]*)[.](0|[1-9][0-9]*)[.](0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?([+][0-9A-Za-z.-]+)?"([ \t]*#.*)?$/) invalid = 1
                sub(/^"/, "", value)
                sub(/".*/, "", value)
                package_version = value
            }
        }
        END {
            if (invalid || name_count != 1 || version_count != 1) exit 1
            print package_name
            print package_version
        }
    ' "$1"
}

manifest_for() {
    printf '%s\n' "$MANIFEST_MAP" | awk -v name="$1" -F: '$1 == name { print substr($0, length($1) + 2); found = 1; exit } END { exit !found }'
}

name_for() {
    printf '%s\n' "$FILE_MAP" | awk -v file="$1" -F: '$1 == file { print substr($0, length($1) + 2); found = 1; exit } END { exit !found }'
}

map_get() {
    printf '%s\n' "$1" | awk -v key="$2" -F= '$1 == key { print substr($0, length($1) + 2); found = 1; exit } END { exit !found }'
}

registry_latest() {
    # Prints the latest registry version of $1, or nothing when the crate
    # has never been published. A failed query still fails loudly.
    search_result=$(cargo search "$1" --limit 1) || fail "could not query the default Cargo registry"
    printf '%s\n' "$search_result" | awk -v package_name="$1" '
        $1 == package_name && $2 == "=" && $3 ~ /^".+"$/ {
            version = substr($3, 2, length($3) - 2)
            matches++
        }
        END {
            if (matches > 1) exit 1
            if (matches == 1) print version
        }
    ' || fail "could not parse the registry response for $1"
}

version_at_least() {
    awk -v left="$1" -v right="$2" '
        function components(value, result) {
            sub(/[+].*$/, "", value)
            sub(/-.*/, "", value)
            return split(value, result, "[.]")
        }
        function compare_component(left_part, right_part) {
            if (length(left_part) < length(right_part)) return -1
            if (length(left_part) > length(right_part)) return 1
            if (("x" left_part) < ("x" right_part)) return -1
            if (("x" left_part) > ("x" right_part)) return 1
            return 0
        }
        BEGIN {
            components(left, left_parts)
            components(right, right_parts)
            for (part = 1; part <= 3; part++) {
                comparison = compare_component(left_parts[part], right_parts[part])
                if (comparison != 0) exit (comparison > 0 ? 0 : 1)
            }
            exit 0
        }
    '
}

plan_next() {
    awk -v current="$1" -v latest="$2" '
        function valid_version(value) {
            return value ~ /^(0|[1-9][0-9]*)[.](0|[1-9][0-9]*)[.](0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?([+][0-9A-Za-z.-]+)?$/
        }
        function components(value, result) {
            sub(/[+].*$/, "", value)
            sub(/-.*/, "", value)
            return split(value, result, "[.]")
        }
        function compare_component(left, right) {
            if (length(left) < length(right)) return -1
            if (length(left) > length(right)) return 1
            if (("x" left) < ("x" right)) return -1
            if (("x" left) > ("x" right)) return 1
            return 0
        }
        function compare_versions(left, right, left_parts, right_parts, part, comparison) {
            components(left, left_parts)
            components(right, right_parts)
            for (part = 1; part <= 3; part++) {
                comparison = compare_component(left_parts[part], right_parts[part])
                if (comparison != 0) return comparison
            }
            return 0
        }
        function increment_decimal(value, result, carry, position, digit, next_digit) {
            result = ""
            carry = 1
            for (position = length(value); position > 0; position--) {
                digit = substr(value, position, 1)
                next_digit = index("0123456789", digit) - 1 + carry
                if (next_digit == 10) {
                    next_digit = 0
                    carry = 1
                } else {
                    carry = 0
                }
                result = substr("0123456789", next_digit + 1, 1) result
            }
            if (carry) result = "1" result
            return result
        }
        BEGIN {
            if (!valid_version(current)) exit 1
            base = current
            if (latest != "") {
                if (!valid_version(latest)) exit 1
                if (compare_versions(current, latest) < 0) base = latest
            }
            components(base, base_parts)
            print base_parts[1] "." increment_decimal(base_parts[2]) ".0"
        }
    '
}

publish_package() {
    # Publishes $1 at $2 unless the registry already has it, so a re-run
    # after a partial publish finishes the remaining crates.
    already_published=$(registry_latest "$1") || exit 1
    if [ -n "$already_published" ] && version_at_least "$already_published" "$2"; then
        printf '%s\n' "$1 $2 is already published; skipping"
        return 0
    fi
    printf '%s\n' "Publishing $1 $2"
    cargo publish -p "$1"
}

ensure_tag() {
    tag=$1-v$2
    case "$newline$tags_at_head$newline" in
        *"$newline$tag$newline"*) ;;
        *) git tag -a "$tag" -m "$1 $2" ;;
    esac
}

ensure_clean_tree() {
    working_changes=$(git status --porcelain=v1) || fail "could not inspect the working tree"
    [ -z "$working_changes" ] || fail "release requires a clean working tree"
}

# Derive the member manifests from the workspace so new crates are picked
# up without editing this script.
member_dirs=$(awk '
    /^[ \t]*\[workspace\][ \t]*$/ { section = "workspace"; next }
    /^[ \t]*\[/ { section = ""; next }
    section == "workspace" {
        line = $0
        if (!capturing) {
            if (line !~ /^[ \t]*members[ \t]*=[ \t]*\[/) next
            capturing = 1
            sub(/^[^\[]*\[/, "", line)
        }
        rest = line
        while (match(rest, /"[^"]*"/)) {
            print substr(rest, RSTART + 1, RLENGTH - 2)
            rest = substr(rest, RSTART + RLENGTH)
        }
        if (line ~ /\]/) capturing = 0
    }
' Cargo.toml) || fail "could not read workspace members"
[ -n "$member_dirs" ] || fail "workspace has no members"

MANIFEST_PATHS="Cargo.toml"
for dir in $member_dirs; do
    case "$dir" in
        *[!A-Za-z0-9_./-]* | '') fail "unsupported member path: $dir" ;;
    esac
    file=$dir/Cargo.toml
    [ -f "$file" ] || fail "workspace member has no manifest: $file"
    MANIFEST_PATHS="$MANIFEST_PATHS $file"
done

MANIFEST_MAP=
FILE_MAP=
PKG_SPACE=
PKG_NAMES=
LOCAL_VERS=
RESET_PATHS="Cargo.toml Cargo.lock"
for file in $MANIFEST_PATHS; do
    metadata=$(read_package_metadata "$file") || fail "expected literal name and semantic version in [package] of $file"
    name=$(printf '%s\n' "$metadata" | sed -n '1p')
    version=$(printf '%s\n' "$metadata" | sed -n '2p')
    [ -n "$name" ] && [ -n "$version" ] || fail "package metadata is incomplete in $file"
    MANIFEST_MAP="${MANIFEST_MAP}${MANIFEST_MAP:+$newline}$name:$file"
    FILE_MAP="${FILE_MAP}${FILE_MAP:+$newline}$file:$name"
    PKG_SPACE="${PKG_SPACE}${PKG_SPACE:+ }$name"
    PKG_NAMES="${PKG_NAMES}${PKG_NAMES:+$newline}$name"
    LOCAL_VERS="${LOCAL_VERS}${LOCAL_VERS:+$newline}$name=$version"
    case "$file" in
        Cargo.toml) ;;
        *) RESET_PATHS="$RESET_PATHS $file" ;;
    esac
done
printf '%s\n' "$MANIFEST_MAP" | awk -F: '{ if ($1 in seen) duplicated = 1; seen[$1] = 1 } END { exit duplicated }' \
    || fail "duplicate package names in workspace manifests"

# Derive inter-crate edges from the manifests. A renamed member dependency
# (`foo = { package = "h12tiny-..." }`) would silently escape the version
# rewrite below, so reject it loudly instead.
EDGES=
for file in $MANIFEST_PATHS; do
    if grep -q 'package *= *"h12tiny-' "$file"; then
        fail "renamed workspace dependency in $file is not supported"
    fi
    own=$(name_for "$file") || fail "no package name for $file"
    for dep in $PKG_SPACE; do
        [ "$dep" = "$own" ] && continue
        if grep -q "^[[:space:]]*${dep}[[:space:]]*=" "$file"; then
            EDGES="${EDGES}${EDGES:+$newline}$own $dep"
        fi
    done
done

# Order crates dependencies-first. Emitted in sorted order among ready
# crates so the sequence is stable.
TOPO_ORDER=$(printf '%s\n' "$EDGES" | awk -v nodes="$PKG_SPACE" '
    BEGIN {
        count = split(nodes, list, " ")
        for (i = 1; i <= count; i++) {
            node = list[i]
            ranked[i] = node
            depcount[node] = 0
            dependents[node] = ""
        }
        for (i = 1; i <= count; i++)
            for (j = i + 1; j <= count; j++)
                if (ranked[j] < ranked[i]) {
                    tmp = ranked[i]
                    ranked[i] = ranked[j]
                    ranked[j] = tmp
                }
    }
    NF == 2 {
        dependent = $1
        dep = $2
        if (!(dependent in depcount) || !(dep in depcount)) {
            printf "%s\n", "unknown package in edge: " $1 " " $2 > "/dev/stderr"
            bad = 1
            next
        }
        dependents[dep] = dependents[dep] " " dependent
        depcount[dependent]++
    }
    END {
        if (bad) exit 1
        emitted = 0
        while (emitted < count) {
            progress = 0
            for (i = 1; i <= count; i++) {
                node = ranked[i]
                if (!done[node] && depcount[node] == 0) {
                    print node
                    done[node] = 1
                    emitted++
                    progress = 1
                    users = dependents[node]
                    user_count = split(users, user_list, " ")
                    for (k = 1; k <= user_count; k++)
                        if (user_list[k] != "") depcount[user_list[k]]--
                }
            }
            if (!progress) {
                print "dependency cycle detected" > "/dev/stderr"
                exit 1
            }
        }
    }
') || fail "could not order workspace crates"

# Crates without workspace dependencies can be dry-run before the release
# commit; anything else must wait until its dependencies are published.
LEAVES=$(printf '%s\n' "$EDGES" | awk -v nodes="$PKG_SPACE" '
    BEGIN {
        count = split(nodes, list, " ")
        for (i = 1; i <= count; i++) hasdep[list[i]] = 0
    }
    NF == 2 { hasdep[$1] = 1 }
    END {
        for (i = 1; i <= count; i++)
            if (!hasdep[list[i]]) print list[i]
    }
')

PUBLISHED=
for name in $PKG_SPACE; do
    latest=$(registry_latest "$name")
    PUBLISHED="${PUBLISHED}${PUBLISHED:+$newline}$name=$latest"
done

# Cargo checks registry ownership before the script edits any release file.
# Ownership of a never-published name cannot be queried; the publish step
# surfaces that instead.
for name in $PKG_SPACE; do
    published=$(map_get "$PUBLISHED" "$name" "=") || fail "no registry state for $name"
    if [ -n "$published" ]; then
        cargo owner --list "$name"
    fi
done

git_subject=$(git log -1 --format=%s) || fail "could not read the current commit subject"
tags_at_head=$(git tag --points-at HEAD) || fail "could not read tags at HEAD"

if [ "$git_subject" = "$release_subject" ]; then
    ensure_clean_tree
    for name in $TOPO_ORDER; do
        local_version=$(map_get "$LOCAL_VERS" "$name" "=") || fail "no local version for $name"
        ensure_tag "$name" "$local_version"
    done
    pending=0
    for name in $TOPO_ORDER; do
        local_version=$(map_get "$LOCAL_VERS" "$name" "=") || fail "no local version for $name"
        already=$(registry_latest "$name") || exit 1
        if [ -n "$already" ] && version_at_least "$already" "$local_version"; then
            printf '%s\n' "$name $local_version is already published; skipping"
        else
            pending=1
        fi
    done
    if [ "$pending" -eq 0 ]; then
        printf '%s\n' "all workspace crates already published; nothing to do"
        exit 0
    fi
    printf '%s\n' "Retrying publish of unpublished workspace crates"
    for name in $TOPO_ORDER; do
        local_version=$(map_get "$LOCAL_VERS" "$name" "=") || fail "no local version for $name"
        publish_package "$name" "$local_version"
    done
    exit 0
fi

NEXT_VERS=
for name in $TOPO_ORDER; do
    local_version=$(map_get "$LOCAL_VERS" "$name" "=") || fail "no local version for $name"
    published=$(map_get "$PUBLISHED" "$name" "=") || fail "no registry state for $name"
    if [ -n "$published" ]; then
        version_at_least "$published" "$local_version" \
            || fail "local $name $local_version is newer than published $published, but HEAD is not the matching release commit"
    fi
    next_version=$(plan_next "$local_version" "$published") || fail "could not plan next version for $name"
    NEXT_VERS="${NEXT_VERS}${NEXT_VERS:+$newline}$name=$next_version"
done

existing_tags=$(git tag) || fail "could not list Git tags"
for name in $TOPO_ORDER; do
    next_version=$(map_get "$NEXT_VERS" "$name" "=") || fail "no planned version for $name"
    next_tag=$name-v$next_version
    case "$newline$existing_tags$newline" in
        *"$newline$next_tag$newline"*) fail "tag $next_tag already exists away from the release commit" ;;
    esac
done
ensure_clean_tree

release_body=$(for name in $PKG_SPACE; do
    printf '%s %s -> %s\n' "$name" "$(map_get "$LOCAL_VERS" "$name" "=")" "$(map_get "$NEXT_VERS" "$name" "=")"
done | sort)

backup_dir_candidate=${TMPDIR:-/tmp}/bump_minor_release.$$
(umask 077 && mkdir "$backup_dir_candidate") || fail "could not create private release backups"
backup_dir=$backup_dir_candidate
for name in $PKG_SPACE; do
    file=$(manifest_for "$name") || fail "no manifest for $name"
    cp -p "$ROOT/$file" "$backup_dir/$name.toml" || fail "could not back up $file"
done
cp -p "$ROOT/Cargo.lock" "$backup_dir/Cargo.lock" || fail "could not back up Cargo.lock"
release_started=1

bump_manifest_version() {
    temporary_manifest_candidate=$ROOT/.Cargo.toml.bump.$$
    (umask 077 && set -C && : > "$temporary_manifest_candidate") || fail "could not create temporary manifest"
    temporary_manifest=$temporary_manifest_candidate
    cp -p "$1" "$temporary_manifest" || fail "could not preserve Cargo.toml permissions"
    awk -v old="$2" -v new="$3" '
        /^[ \t]*\[package\][ \t]*(#.*)?$/ { section = "package"; print; next }
        /^[ \t]*\[/ { section = ""; print; next }
        section == "package" {
            line = $0
            sub(/^[ \t]*/, "", line)
            sub(/[ \t]*$/, "", line)
            if (line ~ /^version[ \t]*=/) {
                matches++
                if (line != "version = \"" old "\"") exit 1
                print "version = \"" new "\""
                next
            }
        }
        { print }
        END { if (matches != 1) exit 1 }
    ' "$1" > "$temporary_manifest" || fail "$1 version changed or could not be updated"
    mv "$temporary_manifest" "$1" || fail "could not install updated $1"
    temporary_manifest=
}

for name in $PKG_SPACE; do
    file=$(manifest_for "$name") || fail "no manifest for $name"
    old_version=$(map_get "$LOCAL_VERS" "$name" "=") || fail "no local version for $name"
    new_version=$(map_get "$NEXT_VERS" "$name" "=") || fail "no planned version for $name"
    bump_manifest_version "$ROOT/$file" "$old_version" "$new_version"
done

# Track each dependency's new version in every dependent manifest. The
# requirement operator (bare, =, ^, ~) is preserved; anything else fails.
rewrite_requirement() {
    awk -v dep="$2" -v old="$3" -v new="$4" '
        BEGIN {
            prefixes[1] = ""
            prefixes[2] = "="
            prefixes[3] = "^"
            prefixes[4] = "~"
        }
        {
            line = $0
            stripped = line
            sub(/^[ \t]*/, "", stripped)
            if (stripped ~ ("^" dep "[ \t]*=")) {
                matched++
                total = 0
                for (k = 1; k <= 4; k++) {
                    want = "version = \"" prefixes[k] old "\""
                    give = "version = \"" prefixes[k] new "\""
                    out = ""
                    rest = line
                    found = 0
                    while ((pos = index(rest, want)) > 0) {
                        found++
                        out = out substr(rest, 1, pos - 1) give
                        rest = substr(rest, pos + length(want))
                    }
                    if (found > 0) {
                        total += found
                        line = out rest
                    }
                }
                if (total != 1) exit 1
                print line
                next
            }
            print
        }
        END { if (matched < 1) exit 1 }
    ' "$1" > "$temporary_manifest" || fail "dependency $2 in $1 changed or could not be updated"
    mv "$temporary_manifest" "$1" || fail "could not install updated $1"
    temporary_manifest=
}

for file in $MANIFEST_PATHS; do
    for dep in $PKG_SPACE; do
        dep_file=$(manifest_for "$dep") || fail "no manifest for $dep"
        [ "$dep_file" = "$file" ] && continue
        old_dep=$(map_get "$LOCAL_VERS" "$dep" "=") || fail "no local version for $dep"
        new_dep=$(map_get "$NEXT_VERS" "$dep" "=") || fail "no planned version for $dep"
        [ "$new_dep" = "$old_dep" ] && continue
        # Only manifests that actually depend on the crate carry a
        # requirement to rewrite; the strict updater still fails on any
        # line it does not recognize.
        if ! grep -q "^[[:space:]]*${dep}[[:space:]]*=" "$ROOT/$file"; then
            continue
        fi
        temporary_manifest_candidate=$ROOT/.Cargo.toml.bump.$$
        (umask 077 && set -C && : > "$temporary_manifest_candidate") || fail "could not create temporary manifest"
        temporary_manifest=$temporary_manifest_candidate
        cp -p "$ROOT/$file" "$temporary_manifest" || fail "could not preserve Cargo.toml permissions"
        rewrite_requirement "$ROOT/$file" "$dep" "$old_dep" "$new_dep"
    done
done

cargo check --workspace --quiet
for name in $LEAVES; do
    cargo publish -p "$name" --dry-run --allow-dirty
done
# shellcheck disable=SC2086
git add -- $RESET_PATHS
git commit -m "$release_subject" -m "$release_body"
release_committed=1
for name in $TOPO_ORDER; do
    next_version=$(map_get "$NEXT_VERS" "$name" "=") || fail "no planned version for $name"
    ensure_tag "$name" "$next_version"
done

# Dependencies must reach the registry before their dependents can even be
# dry-run, so each crate is validated and published in dependency order
# after the release commit.
for name in $TOPO_ORDER; do
    next_version=$(map_get "$NEXT_VERS" "$name" "=") || fail "no planned version for $name"
    cargo publish -p "$name" --dry-run --allow-dirty
    publish_package "$name" "$next_version"
done
