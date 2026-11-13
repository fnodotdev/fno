# One version compare for the shell hooks: version_below_floor <version> <floor>
# answers 0 when version is strictly below floor. sort -V orders the suffix
# shapes correctly on GNU and BSD sort alike (0.4.9rc1 sorts above 0.4.5), and
# an install is below its floor exactly when it sorts first and the two
# differ. The inverted read warned every healthy install (0.4.8 nagged
# against a 0.4.5 floor on 2026-11-13) and stayed silent on stale ones.
# Empty inputs answer not-below: a broken read may only under-warn.
version_below_floor() {
    [[ -n "${1:-}" && -n "${2:-}" ]] || return 1
    [[ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -1)" == "$1" && "$1" != "$2" ]]
}
