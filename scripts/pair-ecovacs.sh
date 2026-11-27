#!/usr/bin/env bash
# Send Ecovacs credentials directly to the Moli CLI, with terminal echo off.
set +x
set -euo pipefail
cd -- "$(dirname -- "$0")/.."

exec 3<>/dev/tty
IFS= read -rs -u 3 -p 'ECOVACS HOME email: ' ecovacs_email
printf '\n' >&3
IFS= read -rs -u 3 -p 'ECOVACS HOME password: ' ecovacs_password
printf '\n' >&3

coproc ECOVACS_PAIR { docker compose exec -T moli-os /moli-os ecovacs-pair; }
pair_pid=$ECOVACS_PAIR_PID
exec {pair_in}>&"${ECOVACS_PAIR[1]}" {pair_out}<&"${ECOVACS_PAIR[0]}"
printf '%s\n' "$ecovacs_email" "$ecovacs_password" >&"$pair_in"
unset ecovacs_email ecovacs_password

if IFS= read -r -u "$pair_out" line; then
    if [[ $line == verification_required ]]; then
        IFS= read -rs -u 3 -p 'Code from the Ecovacs email: ' ecovacs_code
        printf '\n' >&3
        printf '%s\n' "$ecovacs_code" >&"$pair_in"
        unset ecovacs_code
    else
        printf '%s\n' "$line"
    fi
fi
exec {pair_in}>&-
cat <&"$pair_out"
wait "$pair_pid"
docker compose restart moli-os
