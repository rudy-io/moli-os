#!/usr/bin/env bash
# Leaves an Android build of the app on the house, which then offers it to
# the paired phones and installs it on a wall tablet it runs (src/update.ts,
# crates/moli-api/src/appli.rs).
#
#   ./publish-android.sh <moli.apk | https://…/build.apk> <data-folder>
#
# <data-folder>: the house's data folder (next to home.json), local or
# `user@host:/path` (scp). It receives `app/moli.apk` and `app/android.json`
# (versionCode, versionName, SHA-256). The versionCode is read from the APK
# (aapt2 of the Android SDK, or ANDROID_HOME), so the house offers it only to
# older apps. The APK must be signed with the same key as the installed app
# (EAS's), or Android refuses the update.
set -euo pipefail

src=${1:?usage: publish-android.sh <apk|url> <data-folder>}
dest=${2:?usage: publish-android.sh <apk|url> <data-folder>}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/app"

if [[ $src == https://* ]]; then
  curl -fsSL -o "$work/app/moli.apk" "$src"
else
  cp "$src" "$work/app/moli.apk"
fi

sdk=${ANDROID_HOME:-$HOME/Android/Sdk}
aapt2=$(ls -1 "$sdk"/build-tools/*/aapt2 2>/dev/null | sort -V | tail -n 1 || true)
[[ -x $aapt2 ]] || { echo "aapt2 not found (Android SDK build-tools, ANDROID_HOME)" >&2; exit 1; }
# All of it, then its first line (cut short, aapt2 would fail on a broken pipe).
badging=$("$aapt2" dump badging "$work/app/moli.apk")
badging=${badging%%$'\n'*}
code=$(sed -nE "s/.*versionCode='([0-9]+)'.*/\1/p" <<<"$badging")
name=$(sed -nE "s/.*versionName='([^']*)'.*/\1/p" <<<"$badging")
[[ -n $code ]] || { echo "no versionCode in the APK" >&2; exit 1; }
sum=$(sha256sum "$work/app/moli.apk" | cut -d' ' -f1)
# Written as JSON: the version name escaped (a quote or a backslash in it
# would break the file the house serves).
name_json=$(printf '%s' "$name" | sed 's/\\/\\\\/g; s/"/\\"/g' | tr -d '\000-\037')
printf '{\n  "version_code": %s,\n  "version_name": "%s",\n  "sha256": "%s"\n}\n' "$code" "$name_json" "$sum" >"$work/app/android.json"

# The APK first, the manifest last: a phone never reads a manifest whose
# file is not there yet.
if [[ $dest == *:* ]]; then
  ssh "${dest%%:*}" "mkdir -p '${dest#*:}/app'"
  scp -q "$work/app/moli.apk" "$dest/app/moli.apk.new"
  ssh "${dest%%:*}" "mv '${dest#*:}/app/moli.apk.new' '${dest#*:}/app/moli.apk'"
  scp -q "$work/app/android.json" "$dest/app/android.json"
else
  mkdir -p "$dest/app"
  cp "$work/app/moli.apk" "$dest/app/moli.apk.new" && mv "$dest/app/moli.apk.new" "$dest/app/moli.apk"
  cp "$work/app/android.json" "$dest/app/android.json"
fi
echo "published versionCode $code ($name), sha256 $sum"
