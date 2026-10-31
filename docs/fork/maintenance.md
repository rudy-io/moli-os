# Maintaining the fork

`main` stays an unmodified mirror of `rudy-io/moli-os`. `fork/integration` is
the product branch. `feature/*` branches contain isolated integrations or UI
changes; `update/*` branches contain reviewed upstream merges. Do not force-push
published product branches. Tag a tested release before deploying it.

Keep protocol code in its own crate/catalogue package, configuration outside the
repository and shared registry edits small. This reduces the files that collide
with upstream changes. It does not guarantee conflict-free future merges.

## Bringing in upstream

From a clean checkout with a local `fork/integration` branch:

```sh
git remote -v
# Add once if absent; origin must remain your own fork.
git remote add upstream https://github.com/rudy-io/moli-os.git
python3 scripts/fork-update.py --branch update/upstream-YYYY-MM-DD
```

The helper fetches upstream `main`, creates the requested branch from
`fork/integration`, and prepares a merge **without committing it**. It refuses
dirty trees, existing update branches and a `main` integration base. It never
pushes, resets, rebases, changes `main` or touches the running house. If already
up to date, it creates no branch. On conflict, resolve on the update branch or
use `git merge --abort`; do not discard unrelated work.

Review upstream changes, especially driver contracts, guards and device IDs.
Run the project's dev-image checks and frontend checks before committing:

```sh
docker build -t moli-os-dev -f docker/dev.Dockerfile .
docker run --rm -v "$PWD:/src" -v moli-cargo:/usr/local/cargo/registry \
  -v moli-target:/target -w /src -e CARGO_PROFILE_DEV_DEBUG=0 \
  -e CARGO_PROFILE_TEST_DEBUG=0 -e CARGO_INCREMENTAL=0 \
  moli-os-dev sh scripts/check.sh
python3 -m unittest discover -s scripts/tests -v
npm --prefix web ci
npm --prefix web run build
npm --prefix web test
node scripts/i18n-check.mjs
```

Commit the merge, push only the update branch to `origin`, and review a pull
request targeting `fork/integration`. Merge only after checks and review pass.
The helper itself is exercised against disposable Git repositories by its tests.

Update the mirror separately with a fast-forward only, from the `main`
checkout: `git fetch upstream`, then `git merge --ff-only upstream/main`.
Never merge fork features into `main`. A merge of upstream into the product
branch preserves the common history for the next update.

## Release and migration evidence

Record the exact revision, check results and tested device models. Build both
target architectures when publishing an image. Start on a separate Linux
instance with writes disabled, compare states, then test one neutral device
under supervision. The existing installation remains the rollback path until
every required use case has a witnessed acceptance result. A simulator pass,
successful build or accepted command is not evidence of physical execution.

No repository example contains private host configuration, tokens, personal
names or real device identifiers. Supply credentials through the encrypted
vault as described by each integration's onboarding document.
