#!/bin/sh
# Garde anti-fuite : aucune clé, aucun identifiant réel n'entre dans le dépôt.
#
#   scripts/leak-check.sh             l'arbre de travail (hors target, data, node_modules, dist)
#   scripts/leak-check.sh <rév>…      le contenu de ces commits (crochet pre-push)
#
# Deux listes :
# - des motifs génériques, pour tout le monde : clés privées, jetons d'API, adresses MAC et
#   e-mails réels (les exemples restent permis : aa:bb:cc:dd:ee:ff, user@example.com) ;
# - une liste noire privée, hors du dépôt, quand elle existe : $MOLI_DENYLIST, sinon
#   ../moli-maison/leak-denylist.txt. Ce que votre maison seule connaît (prénoms, domaine,
#   adresses du réseau, identifiants d'appareils). Une expression régulière étendue par ligne,
#   insensible à la casse ; lignes vides et commentaires (#) ignorés.
# Et ses exceptions, à côté : $MOLI_ALLOWLIST, sinon ../moli-maison/leak-allow.txt, des
# expressions qui s'appliquent à l'occurrence entière (`chemin:ligne:texte`), par exemple
# le nom de l'auteur dans NOTICE et nulle part ailleurs.
set -eu
cd "$(dirname "$0")/.."
self=scripts/leak-check.sh
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cat >"$tmp/generic" <<'EOF'
-----BEGIN [A-Z ]*PRIVATE KEY-----
(^|[^A-Za-z0-9])sk-(proj-|ant-)?[A-Za-z0-9_-]{20,}
gh[pousr]_[A-Za-z0-9]{30,}
github_pat_[A-Za-z0-9_]{20,}
xox[abpr]-[A-Za-z0-9-]{10,}
AKIA[0-9A-Z]{16}
AIza[0-9A-Za-z_-]{35}
[0-9]{8,10}:AA[A-Za-z0-9_-]{33}
eyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}
[0-9A-Fa-f]{2}([:-][0-9A-Fa-f]{2}){5}
[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}
EOF
# Examples everyone may write: documentation MACs and addresses, placeholders.
allowed='sk-(proj-)?abcdefghij|aa[:-]?bb[:-]?cc[:-]?dd[:-]?ee[:-]?[0-9a-f]{2}|00([:-]00){5}|ff([:-]ff){5}|02([:-]00){4}[:-][0-9a-f]{2}|@(example|exemple)\.(com|org|net|fr)|@localhost|@test([^a-z.]|$)|noreply@anthropic\.com|git@github\.com|testrealm@host\.com'

denylist=${MOLI_DENYLIST:-../moli-maison/leak-denylist.txt}
: >"$tmp/private"
if [ -f "$denylist" ]; then
    # Written on Windows maybe: a trailing CR would make every pattern miss.
    tr -d '\r' <"$denylist" | grep -v -E '^[[:space:]]*(#|$)' >"$tmp/private" || true
fi
allowlist=${MOLI_ALLOWLIST:-../moli-maison/leak-allow.txt}
: >"$tmp/exceptions"
if [ -f "$allowlist" ]; then
    tr -d '\r' <"$allowlist" | grep -v -E '^[[:space:]]*(#|$)' >"$tmp/exceptions" || true
fi

# Prints `path:line:match` for every hit of the pattern file $1 (-i when $2 = i).
scan() {
    [ -s "$1" ] || return 0
    patterns=$1
    case_flag=${2:-}
    if [ $# -gt 2 ]; then
        shift 2
        for rev in "$@"; do
            checked git grep -n -o -E --text ${case_flag:+-i} -f "$patterns" "$rev" \
                -- . ":(exclude)$self"
            # The message travels with the commit too.
            git log -1 --format=%B "$rev" >"$tmp/message"
            status=0
            grep -n -o -E ${case_flag:+-i} -f "$patterns" "$tmp/message" >"$tmp/found" || status=$?
            [ "$status" -le 1 ] || { echo "leak-check : la recherche a échoué (message de $rev)" >&2; exit 2; }
            sed "s|^|$rev:(message):|" "$tmp/found"
        done
    elif git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        # What could be committed: tracked files and untracked ones git does not ignore.
        checked git grep -n -o -E --text ${case_flag:+-i} -f "$patterns" --untracked \
            -- . ":(exclude)$self"
    else
        # No usable git (a worktree seen from a container, an image without git):
        # the tree, minus what .gitignore keeps out. POSIX find + grep (BusyBox too).
        find . \( -name .git -o -name target -o -name data -o -name node_modules \
            -o -name dist \) -prune -o -type f ! -name '*.bak-*' ! -name house.json \
            ! -name leak-check.sh -print0 |
            xargs -0 grep -H -n -o -E ${case_flag:+-i} -f "$patterns" 2>"$tmp/err" || true
        if [ -s "$tmp/err" ]; then
            echo "leak-check : la recherche a échoué, rien n'est garanti :" >&2
            cat "$tmp/err" >&2
            exit 2
        fi
    fi
}

# A search that fails must never pass for a clean one: grep says 0 (found),
# 1 (nothing), anything else is an error.
checked() {
    status=0
    "$@" 2>"$tmp/err" || status=$?
    if [ "$status" -gt 1 ] || [ -s "$tmp/err" ]; then
        echo "leak-check : la recherche a échoué (code $status), rien n'est garanti :" >&2
        cat "$tmp/err" >&2
        exit 2
    fi
}

# Never in a pipeline: a failed search must stop the script, not a subshell.
scan "$tmp/generic" "" "$@" >"$tmp/generic.hits"
scan "$tmp/private" i "$@" >"$tmp/private.hits"
{
    grep -v -i -E "$allowed" "$tmp/generic.hits" || true
    cat "$tmp/private.hits"
} >"$tmp/all"
if [ -s "$tmp/exceptions" ]; then
    grep -v -i -E -f "$tmp/exceptions" "$tmp/all" >"$tmp/hits" || true
else
    mv "$tmp/all" "$tmp/hits"
fi

if [ -s "$tmp/hits" ]; then
    sort -u "$tmp/hits" >"$tmp/unique"
    total=$(wc -l <"$tmp/unique" | tr -d ' ')
    echo "leak-check : $total occurrence(s) à retirer :" >&2
    # A bounded report: a huge one could fill a pipe and stall a git hook.
    head -n 200 "$tmp/unique" | sed 's/^/  /' >&2
    [ "$total" -le 200 ] || echo "  … et $((total - 200)) autres." >&2
    echo "Remplacer par des valeurs fictives (aa:bb:cc:dd:ee:ff, 192.168.1.x, user@example.com…)." >&2
    exit 1
fi
if [ -s "$tmp/private" ]; then
    echo "leak-check : propre (motifs génériques + liste noire privée)."
else
    echo "leak-check : propre (motifs génériques ; pas de liste noire privée)."
fi
