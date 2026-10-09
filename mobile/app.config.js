// The app's identity (store ids, Expo account) and the house it opens are the
// publisher's, not the project's: they come from `house.json` next to this
// file (never committed; model: house.example.json), else from MOLI_URL.
const fs = require("fs");
const path = require("path");

module.exports = ({ config }) => {
  const file = path.join(__dirname, "house.json");
  const house = fs.existsSync(file) ? JSON.parse(fs.readFileSync(file, "utf8")) : {};
  const url = house.url ?? process.env.MOLI_URL ?? "https://maison.example.org";
  return {
    ...config,
    ...(house.owner && { owner: house.owner }),
    ios: {
      ...config.ios,
      ...(house.bundleId && { bundleIdentifier: house.bundleId }),
      ...(house.appleTeamId && { appleTeamId: house.appleTeamId }),
    },
    android: { ...config.android, ...(house.bundleId && { package: house.bundleId }) },
    extra: {
      ...config.extra,
      moliUrl: url,
      // Hosts the dashboard may move to without leaving the app (suffixes:
      // a redirect must not throw the user out), e.g. a sign-in page.
      allowedSuffixes: house.allowedSuffixes ?? [new URL(url).hostname],
      ...(house.easProjectId && { eas: { projectId: house.easProjectId } }),
    },
  };
};
