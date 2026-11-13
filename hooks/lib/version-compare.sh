# The floor compare for the shell hooks. version_key mirrors
# release_install.rs version_key (X.Y.Z, X.Y.Z-rc.N, X.Y.Z-dev.N; dev < rc <
# stable at the same core; any other shape unparsable) and
# version_below_floor <version> <floor> answers 0 when version is strictly
# below floor. An unparsable version or floor answers not-below, matching
# floor_refusal's unparsable-admits rule, so a broken read may only
# under-warn. The inverted sort -V read this replaces warned every healthy
# install (0.4.8 nagged against a 0.4.5 floor on 2026-11-13), stayed silent
# on stale ones, and put every suffix above its bare base, the reverse of
# the prerelease ranks.
version_key() { # version_key <version> -> "X Y Z RANK N" on stdout, nonzero for any other shape
    local core pre
    if [[ "$1" == *-* ]]; then
        core="${1%%-*}"
        pre="${1#*-}"
    else
        core="$1"
        pre=""
    fi
    local -a parts
    IFS=. read -r -a parts <<<"$core"
    [[ ${#parts[@]} -eq 3 ]] || return 1
    local p
    for p in "${parts[@]}"; do
        [[ "$p" =~ ^[0-9]{1,10}$ ]] || return 1
    done
    local rank n
    if [[ -z "$pre" ]]; then
        rank=2
        n=0
    elif [[ "$pre" =~ ^rc\.[0-9]{1,10}$ ]]; then
        rank=1
        n="${pre#rc.}"
    elif [[ "$pre" =~ ^dev\.[0-9]{1,10}$ ]]; then
        rank=0
        n="${pre#dev.}"
    else
        return 1
    fi
    printf '%d %d %d %d %d\n' "${parts[0]}" "${parts[1]}" "${parts[2]}" "$rank" "$n"
}

version_below_floor() { # version_below_floor <version> <floor>
    [[ -n "${1:-}" && -n "${2:-}" ]] || return 1
    local vk fk
    vk="$(version_key "$1")" || return 1
    fk="$(version_key "$2")" || return 1
    local -a v f
    read -r -a v <<<"$vk"
    read -r -a f <<<"$fk"
    # 10# forces decimal: a leading-zero patch like 08 is an octal error in (( )).
    local i
    for i in 0 1 2 3 4; do
        if (( 10#${v[i]} < 10#${f[i]} )); then return 0; fi
        if (( 10#${v[i]} > 10#${f[i]} )); then return 1; fi
    done
    return 1
}
