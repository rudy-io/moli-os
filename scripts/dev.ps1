# Lance une commande dans l'image de dev (aucune toolchain Rust locale requise).
# Usage : scripts\dev.ps1 cargo test        |  scripts\dev.ps1 sh scripts/check.sh
$root = Split-Path -Parent $PSScriptRoot
# The house's private lists (scripts/leak-check.sh), when a house folder sits
# next to the repository: read-only, never copied into it.
$house = @()
$dir = Join-Path (Split-Path -Parent $root) 'moli-maison'
foreach ($list in @(@('leak-denylist.txt', 'MOLI_DENYLIST'), @('leak-allow.txt', 'MOLI_ALLOWLIST'))) {
  $path = Join-Path $dir $list[0]
  if (Test-Path -LiteralPath $path) {
    $house += @('-v', "${path}:/house/$($list[0]):ro", '-e', "$($list[1])=/house/$($list[0])")
  }
}
docker run --rm `
  -v "${root}:/src" `
  -v moli-cargo:/usr/local/cargo/registry `
  -v moli-target:/target `
  @house `
  -w /src moli-os-dev @args
exit $LASTEXITCODE
