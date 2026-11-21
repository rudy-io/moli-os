// The wall tablet's « home screen » entry (modules/moli-wall): an alias of
// the app's own screen, declared as a home screen but switched off. Only a
// tablet made a wall tablet switches it on; a phone never offers Moli as its
// home screen.
const { withAndroidManifest } = require("expo/config-plugins");

module.exports = function withWall(config) {
  return withAndroidManifest(config, (cfg) => {
    const app = cfg.modResults.manifest.application?.[0];
    const pkg = cfg.android?.package;
    if (!app || !pkg) return cfg;
    // Full names: the module finds it as <package>.WallHome.
    const name = `${pkg}.WallHome`;
    app["activity-alias"] = (app["activity-alias"] ?? []).filter((a) => a.$["android:name"] !== name);
    app["activity-alias"].push({
      $: {
        "android:name": name,
        "android:targetActivity": `${pkg}.MainActivity`,
        "android:enabled": "false",
        "android:exported": "true",
      },
      "intent-filter": [
        {
          action: [{ $: { "android:name": "android.intent.action.MAIN" } }],
          category: [
            { $: { "android:name": "android.intent.category.HOME" } },
            { $: { "android:name": "android.intent.category.DEFAULT" } },
          ],
        },
      ],
    });
    return cfg;
  });
};
